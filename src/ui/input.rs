//! Key routing with dialog priority and double-Ctrl+C quit behavior.

use std::time::{Duration, Instant};

use agent_client_protocol::schema::v1::{
    AuthMethod, PermissionOptionId, RequestPermissionOutcome, SelectedPermissionOutcome,
};
use tern_sdk::keys::Key;

use super::composer::Composer;
use crate::{
    event::UiCmd,
    model::{Model, Turn},
};

pub enum Action {
    None,
    Changed,
    Command(UiCmd),
    AuthTerminal(usize),
    Quit,
}

#[derive(Default)]
pub struct InputState {
    last_interrupt: Option<Instant>,
}

impl InputState {
    pub fn key(&mut self, key: &Key, model: &mut Model, composer: &mut Composer) -> Action {
        if key.is("ctrl+d") && composer.text.is_empty() {
            return Action::Quit;
        }
        if key.is("ctrl+c") {
            if matches!(model.turn, Turn::Running { .. }) {
                model.cancel();
                return Action::Command(UiCmd::Cancel);
            }
            if composer.text.is_empty() {
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
        match key.name.as_str() {
            "enter" if !key.shift && !key.alt => return submit(model, composer),
            "enter" => composer.insert("\n"),
            "paste" => composer.insert(key.text.as_deref().unwrap_or_default()),
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
    if composer.text.trim().is_empty() {
        return Action::None;
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
    model.submitted(text);
    if let Some(crate::model::Entry {
        kind: crate::model::EntryKind::User { blocks, .. },
        ..
    }) = model.transcript.last_mut()
    {
        blocks.extend(
            prompt
                .iter()
                .filter(|block| {
                    matches!(
                        block,
                        agent_client_protocol::schema::v1::ContentBlock::ResourceLink(_)
                    )
                })
                .cloned(),
        );
    }
    Action::Command(UiCmd::Prompt(prompt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_sdk::keys::Decoder;

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
}
