//! Key routing with dialog priority and double-Ctrl+C quit behavior.

use std::{
    io::{self, Read as _},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use agent_client_protocol::schema::v1::{
    AuthMethod, PermissionOptionId, RequestPermissionOutcome, SelectedPermissionOutcome,
};
use tern_sdk::keys::Key;

use super::{
    attach::{self, ImageAttachment, MEDIA_LIMIT},
    composer::Composer,
};
use crate::{
    event::UiCmd,
    model::{Model, Turn},
};

/// Injectable clipboard access keeps key-routing tests independent of the host desktop.
pub trait Clipboard {
    fn image_png(&self) -> io::Result<Option<Vec<u8>>>;
}

pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn image_png(&self) -> io::Result<Option<Vec<u8>>> {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let x11 = std::env::var_os("DISPLAY").is_some();
        let commands: [Option<(&str, &[&str])>; 3] = [
            wayland.then_some(("wl-paste", &["--type", "image/png"])),
            x11.then_some(("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"])),
            (cfg!(target_os = "macos") && !wayland && !x11).then_some(("swift", &["-e", "import AppKit; import Foundation; let board = NSPasteboard.general; if let image = board.data(forType: .png) ?? board.data(forType: .tiff).flatMap({ NSBitmapImageRep(data: $0)?.representation(using: .png, properties: [:]) }) { FileHandle.standardOutput.write(image) }"])),
        ];
        for (program, args) in commands.into_iter().flatten() {
            let Ok(mut child) = Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
            else {
                continue;
            };
            let mut bytes = Vec::new();
            let read = child
                .stdout
                .take()
                .expect("piped clipboard stdout")
                .take(MEDIA_LIMIT + 1)
                .read_to_end(&mut bytes);
            if bytes.len() as u64 > MEDIA_LIMIT {
                let _ = child.kill();
            }
            let status = child.wait()?;
            read?;
            if bytes.len() as u64 > MEDIA_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "clipboard image exceeds 5 MiB limit",
                ));
            }
            if status.success() && !bytes.is_empty() {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }
}

pub fn paste_path(text: &str, model: &mut Model, composer: &mut Composer) -> Option<Action> {
    let path = attach::pasted_image_path(text)?;
    if !model.prompt_capabilities.image {
        model
            .errors
            .push("This agent does not accept images".into());
        return None;
    }
    match ImageAttachment::from_path(&path) {
        Ok(image) => {
            composer.images.push(image);
            Some(Action::Changed)
        }
        Err(error) => {
            model.errors.push(format!("Cannot paste image: {error}"));
            None
        }
    }
}

/// An image pasted in the window (enhanced paste) joins the draft.
pub fn paste_image(image: super::paste::PastedImage, model: &mut Model, composer: &mut Composer) {
    if !model.prompt_capabilities.image {
        model
            .errors
            .push("This agent does not accept images".into());
        return;
    }
    match ImageAttachment::from_clipboard(image.mime_type, image.bytes) {
        Ok(image) => composer.images.push(image),
        Err(error) => model.errors.push(format!("Cannot paste image: {error}")),
    }
}

fn paste_clipboard(
    model: &mut Model,
    composer: &mut Composer,
    clipboard: &impl Clipboard,
) -> Action {
    if !model.prompt_capabilities.image {
        model
            .errors
            .push("This agent does not accept images".into());
        return Action::Changed;
    }
    match clipboard.image_png() {
        Ok(Some(bytes)) => match ImageAttachment::from_clipboard("image/png".into(), bytes) {
            Ok(image) => composer.images.push(image),
            Err(error) => model.errors.push(format!("Cannot paste image: {error}")),
        },
        Ok(None) => model.errors.push("No PNG image in system clipboard".into()),
        Err(error) => model
            .errors
            .push(format!("Cannot read clipboard image: {error}")),
    }
    Action::Changed
}

pub enum Action {
    None,
    Changed,
    Command(UiCmd),
    OpenGoal,
    OpenUsage,
    AuthTerminal(usize),
    Quit,
}

#[derive(Default)]
pub struct InputState {
    last_interrupt: Option<Instant>,
}

impl InputState {
    pub fn key(&mut self, key: &Key, model: &mut Model, composer: &mut Composer) -> Action {
        self.key_with_clipboard(key, model, composer, &SystemClipboard)
    }

    fn key_with_clipboard(
        &mut self,
        key: &Key,
        model: &mut Model,
        composer: &mut Composer,
        clipboard: &impl Clipboard,
    ) -> Action {
        if key.is("ctrl+d") && composer.text.is_empty() && composer.images.is_empty() {
            return Action::Quit;
        }
        if key.is("ctrl+c") {
            if matches!(model.turn, Turn::Running { .. }) {
                model.cancel();
                return Action::Command(UiCmd::Cancel);
            }
            if composer.text.is_empty() && composer.images.is_empty() {
                let now = Instant::now();
                let quit = self
                    .last_interrupt
                    .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1));
                self.last_interrupt = Some(now);
                if quit {
                    return Action::Quit;
                }
            }
            return Action::None;
        }
        self.last_interrupt = None;
        if let Some(permission) = model.oldest_permission() {
            let id = permission.id;
            if key.is("escape") {
                let outcome =
                    model
                        .reject_once()
                        .map_or(RequestPermissionOutcome::Cancelled, |option_id| {
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                option_id,
                            ))
                        });
                return Action::Command(UiCmd::Permission { id, outcome });
            }
            if let Some(index) = digit(key)
                && let Some(option) = permission.request.options.get(index)
            {
                return Action::Command(UiCmd::Permission {
                    id,
                    outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        option.option_id.clone(),
                    )),
                });
            }
            return Action::None;
        }
        if let Some(auth) = &model.auth {
            if let Some(index) = digit(key)
                && let Some(AuthMethod::Agent(method)) = auth.methods.get(index)
            {
                return Action::Command(UiCmd::Authenticate(method.id.clone()));
            }
            return Action::None;
        }
        if model.exited.is_some() && model.restartable && key.is("r") {
            model.exited = None;
            model.session = None;
            model.status = "Restarting".into();
            return Action::Command(UiCmd::Restart);
        }
        if model.exited.is_some() {
            return Action::None;
        }
        if key.is("escape") {
            if matches!(model.turn, Turn::Running { .. }) {
                model.cancel();
                return Action::Command(UiCmd::Cancel);
            }
            return Action::None;
        }
        if key.is("alt+v") || key.is("ctrl+v") {
            return paste_clipboard(model, composer, clipboard);
        }
        match key.name.as_str() {
            "enter" if !key.shift && !key.alt => return submit(model, composer),
            "enter" if key.alt && !key.shift => return submit_queued(model, composer),
            "enter" => composer.insert("\n"),
            "paste" => {
                let text = key.text.as_deref().unwrap_or_default();
                if text.is_empty() {
                    return paste_clipboard(model, composer, clipboard);
                }
                if let Some(action) = paste_path(text, model, composer) {
                    return action;
                }
                composer.insert(text);
            }
            "backspace" => composer.backspace(),
            "delete" => composer.delete(),
            "left" => composer.cursor = composer.cursor.saturating_sub(1),
            "right" => composer.cursor = (composer.cursor + 1).min(composer.text.chars().count()),
            "home" => composer.cursor = 0,
            "end" => composer.cursor = composer.text.chars().count(),
            "up" => {
                if !composer.up() {
                    return Action::None;
                }
            }
            "down" => {
                if !composer.down() {
                    return Action::None;
                }
            }
            _ => {
                if let Some(text) = key.typed() {
                    composer.insert(text);
                } else {
                    return Action::None;
                }
            }
        }
        Action::Changed
    }
}

fn digit(key: &Key) -> Option<usize> {
    if key.ctrl || key.alt || key.meta {
        return None;
    }
    key.name
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=9).contains(n))
        .map(|n| n - 1)
}

pub fn permission(id: u64, option_id: PermissionOptionId) -> UiCmd {
    UiCmd::Permission {
        id,
        outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    }
}

pub fn submit(model: &mut Model, composer: &mut Composer) -> Action {
    submit_with_queue(model, composer, false)
}

pub fn submit_queued(model: &mut Model, composer: &mut Composer) -> Action {
    submit_with_queue(model, composer, true)
}

fn submit_with_queue(model: &mut Model, composer: &mut Composer, queue: bool) -> Action {
    if composer.text.trim().is_empty() && composer.images.is_empty() {
        return Action::None;
    }
    if composer.text.trim() == "/usage" {
        composer.take();
        return Action::OpenUsage;
    }
    let prompt = if let Some(session) = &model.session {
        match super::attach::prompt(
            &composer.text,
            &session.cwd,
            &session.additional_dirs,
            Some(&model.prompt_capabilities),
        ) {
            Ok(blocks) => blocks,
            Err(error) => {
                model.errors.push(format!("Cannot attach file: {error}"));
                return Action::Changed;
            }
        }
    } else {
        vec![agent_client_protocol::schema::v1::ContentBlock::Text(
            agent_client_protocol::schema::v1::TextContent::new(composer.text.clone()),
        )]
    };
    let Some(text) = composer.take() else {
        return Action::None;
    };
    let mut prompt = prompt;
    prompt.extend(
        std::mem::take(&mut composer.images)
            .into_iter()
            .map(ImageAttachment::into_block),
    );
    let steer = !queue && model.ext.steering && matches!(model.turn, Turn::Running { .. });
    if steer {
        model.steered(text, prompt.clone());
        return Action::Command(UiCmd::Steer(prompt));
    }
    model.submitted(text);
    model.attach_to_prompt(
        prompt
            .iter()
            .filter(|block| {
                matches!(
                    block,
                    agent_client_protocol::schema::v1::ContentBlock::ResourceLink(_)
                        | agent_client_protocol::schema::v1::ContentBlock::Image(_)
                        | agent_client_protocol::schema::v1::ContentBlock::Audio(_)
                )
            })
            .cloned()
            .collect(),
    );
    Action::Command(UiCmd::Prompt(prompt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use tern_sdk::keys::Decoder;

    struct FakeClipboard(Option<Vec<u8>>);

    impl Clipboard for FakeClipboard {
        fn image_png(&self) -> io::Result<Option<Vec<u8>>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn pasted_path_and_file_uri_submit_image_when_supported() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("photo space.png");
        std::fs::write(&image, b"png payload").unwrap();
        let mut model = Model::default();
        model.prompt_capabilities.image = true;
        let mut composer = Composer::default();
        let mut input = InputState::default();
        for pasted in [
            image.to_string_lossy().into_owned(),
            format!("file://{}", image.to_string_lossy().replace(' ', "%20")),
        ] {
            let key = Key {
                text: Some(pasted),
                ..Key::named("paste")
            };
            assert!(matches!(
                input.key(&key, &mut model, &mut composer),
                Action::Changed
            ));
            assert!(composer.text.is_empty());
            assert_eq!(composer.images.len(), 1);
            let Action::Command(UiCmd::Prompt(blocks)) = submit(&mut model, &mut composer) else {
                panic!("image-only prompt");
            };
            assert!(blocks.iter().any(|block| matches!(block, agent_client_protocol::schema::v1::ContentBlock::Image(image) if image.mime_type == "image/png" && image.data == base64::engine::general_purpose::STANDARD.encode(b"png payload"))));
            assert!(composer.images.is_empty());
        }
    }

    #[test]
    fn unsupported_agent_gets_notice_and_retains_pasted_path_as_text() {
        let mut model = Model::default();
        let mut composer = Composer::default();
        let path = "/tmp/image.png";
        let key = Key {
            text: Some(path.into()),
            ..Key::named("paste")
        };
        let mut input = InputState::default();
        assert!(matches!(
            input.key(&key, &mut model, &mut composer),
            Action::Changed
        ));
        assert_eq!(composer.text, path);
        assert!(composer.images.is_empty());
        assert!(
            model
                .errors
                .iter()
                .any(|error| error.contains("does not accept images"))
        );
    }
    #[test]
    fn oversized_pasted_image_is_not_attached_and_shows_error() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("huge.png");
        std::fs::File::create(&image)
            .unwrap()
            .set_len(MEDIA_LIMIT + 1)
            .unwrap();
        let mut model = Model::default();
        model.prompt_capabilities.image = true;
        let mut composer = Composer::default();
        let key = Key {
            text: Some(image.to_string_lossy().into_owned()),
            ..Key::named("paste")
        };
        assert!(matches!(
            InputState::default().key(&key, &mut model, &mut composer),
            Action::Changed
        ));
        assert!(composer.images.is_empty());
        assert!(
            model
                .errors
                .iter()
                .any(|error| error.contains("5 MiB limit"))
        );
    }

    #[test]
    fn clipboard_image_uses_injected_reader_and_obeys_agent_capability() {
        let mut model = Model::default();
        let mut composer = Composer::default();
        let key = Key {
            alt: true,
            ..Key::named("v")
        };
        let mut input = InputState::default();
        let reader = FakeClipboard(Some(b"png payload".to_vec()));
        assert!(matches!(
            input.key_with_clipboard(&key, &mut model, &mut composer, &reader),
            Action::Changed
        ));
        assert!(composer.images.is_empty());
        assert!(
            model
                .errors
                .iter()
                .any(|error| error.contains("does not accept images"))
        );
        model.prompt_capabilities.image = true;
        assert!(matches!(
            input.key_with_clipboard(&key, &mut model, &mut composer, &reader),
            Action::Changed
        ));
        assert_eq!(composer.images.len(), 1);
        assert!(
            matches!(submit(&mut model, &mut composer), Action::Command(UiCmd::Prompt(blocks)) if blocks.iter().any(|block| matches!(block, agent_client_protocol::schema::v1::ContentBlock::Image(_))))
        );
        let empty_paste = Key {
            text: Some(String::new()),
            ..Key::named("paste")
        };
        assert!(matches!(
            input.key_with_clipboard(&empty_paste, &mut model, &mut composer, &reader),
            Action::Changed
        ));
        assert_eq!(composer.images.len(), 1);
    }

    #[test]
    fn ctrl_d_exits_after_submit() {
        let mut model = Model::default();
        let mut composer = Composer::default();
        composer.insert("hi");
        assert!(matches!(
            submit(&mut model, &mut composer),
            Action::Command(UiCmd::Prompt(_))
        ));
        let key = Decoder::new().feed(b"\x04").pop().unwrap();
        assert!(key.is("ctrl+d"));
        assert!(matches!(
            InputState::default().key(&key, &mut model, &mut composer),
            Action::Quit
        ));
    }

    #[test]
    fn permission_dialog_answers_oldest_on_digit_and_escape() {
        use agent_client_protocol::schema::v1::{
            PermissionOption, PermissionOptionKind, RequestPermissionRequest, ToolCallUpdate,
            ToolCallUpdateFields,
        };
        let mut model = Model::default();
        model.apply(crate::event::BridgeEvent::Permission {
            id: 10,
            request: Box::new(RequestPermissionRequest::new(
                "session",
                ToolCallUpdate::new("tool", ToolCallUpdateFields::new().title("Edit")),
                vec![
                    PermissionOption::new("yes", "Allow", PermissionOptionKind::AllowOnce),
                    PermissionOption::new("no", "Reject", PermissionOptionKind::RejectOnce),
                ],
            )),
        });
        let mut input = InputState::default();
        let mut composer = Composer::default();
        let Action::Command(UiCmd::Permission { id, outcome }) =
            input.key(&Key::named("1"), &mut model, &mut composer)
        else {
            panic!("answer")
        };
        assert_eq!(id, 10);
        assert_eq!(
            outcome,
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("yes"))
        );
        let Action::Command(UiCmd::Permission { outcome, .. }) =
            input.key(&Key::named("escape"), &mut model, &mut composer)
        else {
            panic!("reject")
        };
        assert_eq!(
            outcome,
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("no"))
        );
        model.submitted("busy".into());
        let cancel = Key {
            ctrl: true,
            ..Key::named("c")
        };
        assert!(matches!(
            input.key(&cancel, &mut model, &mut composer),
            Action::Command(UiCmd::Cancel)
        ));
    }
    #[test]
    fn enter_steers_only_when_supported_and_alt_enter_queues() {
        let mut input = InputState::default();
        let mut model = Model::default();
        let mut composer = Composer::default();
        model.submitted("original".into());
        model.ext.steering = true;
        composer.insert("now");
        assert!(matches!(
            input.key(&Key::named("enter"), &mut model, &mut composer),
            Action::Command(UiCmd::Steer(_))
        ));
        assert_eq!(model.queued, 0);
        assert!(matches!(
            model.transcript.last().unwrap().kind,
            crate::model::EntryKind::Steering { .. }
        ));
        composer.insert("later");
        let alt = Key {
            alt: true,
            ..Key::named("enter")
        };
        assert!(matches!(
            input.key(&alt, &mut model, &mut composer),
            Action::Command(UiCmd::Prompt(_))
        ));
        assert_eq!(model.queued, 1);
        model.ext.steering = false;
        composer.insert("without support");
        assert!(matches!(
            input.key(&Key::named("enter"), &mut model, &mut composer),
            Action::Command(UiCmd::Prompt(_))
        ));
        assert_eq!(model.queued, 2);
    }
}

#[cfg(test)]
mod sent_image_tests {
    use agent_client_protocol::schema::v1::{
        ContentBlock, ContentChunk, SessionUpdate, TextContent,
    };

    use super::*;
    use base64::Engine as _;

    use crate::{event::BridgeEvent, model::EntryKind, ui::paste::PastedImage};

    /// A real 1x1 PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn images_per_prompt(model: &Model) -> Vec<usize> {
        model
            .transcript
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::User { blocks, .. } => Some(
                    blocks
                        .iter()
                        .filter(|block| matches!(block, ContentBlock::Image(_)))
                        .count(),
                ),
                _ => None,
            })
            .collect()
    }

    fn send(model: &mut Model, text: &str) -> ContentBlock {
        let mut composer = Composer::default();
        paste_image(
            PastedImage {
                mime_type: "image/png".into(),
                bytes: base64::engine::general_purpose::STANDARD
                    .decode(PNG)
                    .unwrap(),
            },
            model,
            &mut composer,
        );
        composer.insert(text);
        let Action::Command(UiCmd::Prompt(prompt)) = submit(model, &mut composer) else {
            panic!("prompt sent")
        };
        prompt
            .into_iter()
            .find(|block| matches!(block, ContentBlock::Image(_)))
            .unwrap()
    }

    fn user_chunk(model: &mut Model, block: ContentBlock) {
        model.apply(BridgeEvent::Update(Box::new(
            SessionUpdate::UserMessageChunk(ContentChunk::new(block)),
        )));
    }

    #[test]
    fn a_sent_image_shows_in_the_prompt_and_its_echo_is_absorbed_once() {
        let mut model = Model::default();
        model.prompt_capabilities.image = true;
        let image = send(&mut model, "what is this?");
        assert_eq!(images_per_prompt(&model), [1], "the bubble shows the image");
        user_chunk(
            &mut model,
            ContentBlock::Text(TextContent::new("what is this?")),
        );
        user_chunk(&mut model, image.clone());
        assert_eq!(
            images_per_prompt(&model),
            [1],
            "the echo adds no second copy"
        );
        // The same image arriving again is a new user message, not an echo.
        user_chunk(&mut model, image);
        assert_eq!(images_per_prompt(&model), [1, 1]);
    }

    #[test]
    fn an_image_only_prompt_and_a_repeat_each_keep_their_image() {
        let mut model = Model::default();
        model.prompt_capabilities.image = true;
        let first = send(&mut model, "");
        // Queued behind the first turn: the same image again.
        let second = send(&mut model, "again");
        assert_eq!(first, second);
        assert_eq!(images_per_prompt(&model), [1, 1]);
        // Two echoes, one per submission.
        user_chunk(&mut model, first);
        user_chunk(&mut model, second.clone());
        assert_eq!(images_per_prompt(&model), [1, 1]);
        user_chunk(&mut model, second);
        assert_eq!(
            images_per_prompt(&model),
            [1, 1, 1],
            "a third copy is not an echo"
        );
    }

    #[test]
    fn an_echo_that_never_came_does_not_hide_the_image_later() {
        use agent_client_protocol::schema::v1::{PromptResponse, StopReason};
        let mut model = Model::default();
        model.prompt_capabilities.image = true;
        let image = send(&mut model, "first");
        // The adapter did not echo; the turn ends.
        model.apply(BridgeEvent::TurnEnded(Ok(Box::new(PromptResponse::new(
            StopReason::EndTurn,
        )))));
        user_chunk(&mut model, image);
        assert_eq!(
            images_per_prompt(&model),
            [1, 1],
            "shown, not taken for an echo"
        );
    }
}
