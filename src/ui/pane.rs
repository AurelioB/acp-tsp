//! Pane-level signals beside TSP: the title (OSC 2), working/waiting progress (OSC 9;4) and
//! attention notifications (OSC 99). Tern shows them on tabs and pane headers, as the
//! progress edge and agent working/idle/waiting state, and in the attention inbox.
//!
//! They share the pty with TSP frames; every write here is one whole sequence issued
//! between session calls on the UI thread, so it never splits a frame.
use std::{cell::RefCell, io, rc::Rc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use tern_sdk::term::Terminal;

use crate::model::{Model, Turn, TurnOutcome};

/// The session's terminal, shared so pane signals can be written between frames.
pub struct SharedTerm(Rc<RefCell<Box<dyn Terminal>>>);

impl SharedTerm {
    pub fn new(term: impl Terminal + 'static) -> Self {
        Self(Rc::new(RefCell::new(Box::new(term))))
    }

    fn handle(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Terminal for SharedTerm {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.borrow_mut().write(bytes)
    }
    fn read(&mut self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        self.0.borrow_mut().read(buf, timeout)
    }
    fn restore(&mut self) -> io::Result<()> {
        self.0.borrow_mut().restore()
    }
    fn modes(&mut self, paste: bool, kitty: bool) {
        self.0.borrow_mut().modes(paste, kitty);
    }
}

/// OSC 9;4 states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Clear = 0,
    Working = 3,
    /// Paused: the agent waits for the user (permission, question, sign-in).
    Waiting = 4,
}

pub struct PaneSignals {
    out: SharedTerm,
    title: Option<String>,
    progress: Progress,
    dialog: Option<u64>,
    finished_turns: u64,
    exited: bool,
}

impl PaneSignals {
    pub fn new(term: &SharedTerm) -> Self {
        Self {
            out: term.handle(),
            title: None,
            progress: Progress::Clear,
            dialog: None,
            finished_turns: 0,
            exited: false,
        }
    }

    /// Writes whatever changed since the last call.
    pub fn sync(&mut self, model: &Model) -> io::Result<()> {
        let agent = agent_name(model);
        let title = model
            .session
            .as_ref()
            .and_then(|session| session.title.as_deref())
            .map(clean)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| agent.clone());
        if self.title.as_ref() != Some(&title) {
            self.out.write(format!("\x1b]2;{title}\x1b\\").as_bytes())?;
            self.title = Some(title);
        }
        let progress = progress(model);
        if progress != self.progress {
            self.out
                .write(format!("\x1b]9;4;{};\x1b\\", progress as u8).as_bytes())?;
            self.progress = progress;
        }
        let dialog = model.dialogs.front().copied();
        if dialog != self.dialog {
            // The answered request's notice is stale: close it (the next one gets its own id).
            if let Some(old) = self.dialog {
                self.close(&format!("dialog-{old}"))?;
            }
            if let Some(id) = dialog {
                let what = if model.oldest_permission().is_some() {
                    "Permission needed"
                } else {
                    "Question waiting"
                };
                self.notify(&format!("dialog-{id}"), &agent, what)?;
            }
        }
        self.dialog = dialog;
        if model.finished_turns != self.finished_turns {
            self.finished_turns = model.finished_turns;
            // A finished turn needs no notice: progress clearing is the signal (Tern records
            // `agent-done` for agent blocks itself; a notice would add a second, "waiting" entry).
            let body = match model.last_outcome {
                Some(TurnOutcome::Failed) => Some("Stopped with error".to_owned()),
                Some(TurnOutcome::Stopped(reason)) => Some(format!("Stopped: {reason:?}")),
                Some(TurnOutcome::Done | TurnOutcome::Cancelled) | None => None,
            };
            if let Some(body) = body {
                self.notify("turn", &agent, &body)?;
            }
        }
        let exited = model.exited.is_some();
        if exited && !self.exited {
            self.notify("exit", &agent, "Agent exited")?;
        }
        self.exited = exited;
        Ok(())
    }

    /// Leaves no progress behind when the program quits.
    pub fn clear(&mut self) -> io::Result<()> {
        if self.progress != Progress::Clear {
            self.out.write(b"\x1b]9;4;0;\x1b\\")?;
            self.progress = Progress::Clear;
        }
        Ok(())
    }

    /// An OSC 99 (kitty) notification: the agent as title, `body`, focusing the pane when
    /// clicked. Payloads are base64 (`e=1`), so no text can end the sequence early. A later
    /// notice with the same `id` replaces this one.
    fn notify(&mut self, id: &str, agent: &str, body: &str) -> io::Result<()> {
        let id = format!("acp-tsp-{}-{id}", std::process::id());
        let app = BASE64.encode("acp-tsp");
        let title = BASE64.encode(agent);
        let body = BASE64.encode(clean(body));
        self.out.write(
            format!(
                "\x1b]99;i={id}:f={app}:a=focus:d=0:e=1;{title}\x1b\\\x1b]99;i={id}:p=body:e=1;{body}\x1b\\"
            )
            .as_bytes(),
        )
    }

    /// Withdraws notification `id`.
    fn close(&mut self, id: &str) -> io::Result<()> {
        let id = format!("acp-tsp-{}-{id}", std::process::id());
        self.out
            .write(format!("\x1b]99;i={id}:p=close;\x1b\\").as_bytes())
    }
}

impl Drop for PaneSignals {
    /// Error exits skip [`PaneSignals::clear`]; never leave the pane looking busy.
    fn drop(&mut self) {
        let _ = self.clear();
    }
}

pub fn progress(model: &Model) -> Progress {
    if model.exited.is_some() {
        Progress::Clear
    } else if !model.dialogs.is_empty() || model.auth.is_some() {
        Progress::Waiting
    } else if matches!(model.turn, Turn::Running { .. }) && !model.usage_report.fetching {
        Progress::Working
    } else {
        Progress::Clear
    }
}

fn agent_name(model: &Model) -> String {
    let name = model
        .agent
        .as_ref()
        .and_then(|agent| agent.title.clone())
        .unwrap_or_else(|| model.launch_name.clone());
    let name = clean(&name);
    if name.is_empty() {
        "acp-tsp".into()
    } else {
        name
    }
}

/// One line without control characters (they would end or corrupt the OSC), at most 120
/// characters.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use agent_client_protocol::schema::v1::{
        PermissionOption, PermissionOptionKind, PromptResponse, RequestPermissionRequest,
        SessionInfoUpdate, SessionUpdate, StopReason, ToolCallUpdate, ToolCallUpdateFields,
    };

    use super::*;
    use crate::event::{BridgeEvent, SessionStartKind};

    struct Capture(Rc<RefCell<Vec<u8>>>);
    impl Terminal for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
        fn read(&mut self, _: &mut [u8], _: Option<Duration>) -> io::Result<Option<usize>> {
            Ok(None)
        }
    }

    fn signals() -> (PaneSignals, Rc<RefCell<Vec<u8>>>) {
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let term = SharedTerm::new(Capture(bytes.clone()));
        (PaneSignals::new(&term), bytes)
    }

    fn take(bytes: &Rc<RefCell<Vec<u8>>>) -> String {
        String::from_utf8(std::mem::take(&mut *bytes.borrow_mut())).unwrap()
    }

    fn started() -> Model {
        let mut model = Model::default();
        model.launch_name = "Codex".into();
        model.apply(BridgeEvent::SessionStarted {
            session_id: "s1".into(),
            kind: SessionStartKind::New,
            cwd: "/tmp".into(),
            additional_dirs: vec![],
            modes: None,
            config_options: None,
        });
        model
    }

    fn permission(model: &mut Model, id: u64) {
        model.apply(BridgeEvent::Permission {
            id,
            request: Box::new(RequestPermissionRequest::new(
                "s1",
                ToolCallUpdate::new("t", ToolCallUpdateFields::new()),
                vec![PermissionOption::new(
                    "yes",
                    "Allow",
                    PermissionOptionKind::AllowOnce,
                )],
            )),
        });
    }

    fn end(model: &mut Model, result: Result<StopReason, String>) {
        model.apply(BridgeEvent::TurnEnded(
            result.map(|reason| Box::new(PromptResponse::new(reason))),
        ));
    }

    #[test]
    fn working_waiting_and_done_reach_the_pane() {
        let (mut signals, bytes) = signals();
        let mut model = started();
        signals.sync(&model).unwrap();
        assert_eq!(
            take(&bytes),
            "\x1b]2;Codex\x1b\\",
            "untitled: the agent names the pane"
        );

        model.submitted("hi".into());
        signals.sync(&model).unwrap();
        assert_eq!(take(&bytes), "\x1b]9;4;3;\x1b\\");

        permission(&mut model, 7);
        signals.sync(&model).unwrap();
        let out = take(&bytes);
        assert!(out.starts_with("\x1b]9;4;4;\x1b\\"), "{out:?}");
        assert_eq!(
            notices(&out),
            [(
                "dialog-7".into(),
                "Codex".into(),
                "Permission needed".into()
            )]
        );
        signals.sync(&model).unwrap();
        assert_eq!(take(&bytes), "", "one notification per request");

        model.apply(BridgeEvent::Resolved(7));
        signals.sync(&model).unwrap();
        let out = take(&bytes);
        assert!(out.starts_with("\x1b]9;4;3;\x1b\\"), "{out:?}");
        assert_eq!(
            closed(&out),
            ["dialog-7"],
            "the answered request's notice goes"
        );

        end(&mut model, Ok(StopReason::EndTurn));
        signals.sync(&model).unwrap();
        assert_eq!(
            take(&bytes),
            "\x1b]9;4;0;\x1b\\",
            "done is the progress clearing"
        );
    }

    #[test]
    fn cancelled_turns_stay_quiet_and_errors_notify() {
        let (mut signals, bytes) = signals();
        let mut model = started();
        signals.sync(&model).unwrap();
        take(&bytes);
        model.submitted("one".into());
        model.cancel();
        end(&mut model, Ok(StopReason::Cancelled));
        signals.sync(&model).unwrap();
        assert!(!take(&bytes).contains("]99;"));
        model.submitted("two".into());
        end(&mut model, Err("boom".into()));
        signals.sync(&model).unwrap();
        assert_eq!(
            notices(&take(&bytes)),
            [("turn".into(), "Codex".into(), "Stopped with error".into())]
        );
    }

    #[test]
    fn notice_text_is_encoded_so_it_cannot_end_the_sequence() {
        let (mut signals, bytes) = signals();
        signals
            .notify("x", "Agent\u{7};\u{1b}\\ 🦀", "line\u{1b}]2;evil")
            .unwrap();
        let out = take(&bytes);
        // Exactly the two sequences written: the title part and the body part.
        assert_eq!(out.matches("\x1b]").count(), 2, "{out:?}");
        assert_eq!(
            notices(&out),
            [(
                "x".into(),
                "Agent\u{7};\u{1b}\\ 🦀".into(),
                "line ]2;evil".into()
            )]
        );
    }

    /// Decodes OSC 99 notices as (id suffix, title, body).
    fn notices(out: &str) -> Vec<(String, String, String)> {
        let mut found: Vec<(String, String, String)> = Vec::new();
        for sequence in out.split("\x1b]99;").skip(1) {
            let sequence = sequence.split("\x1b\\").next().unwrap();
            let (meta, payload) = sequence.split_once(';').unwrap();
            let keys: Vec<_> = meta.split(':').collect();
            if keys.contains(&"p=close") {
                continue;
            }
            assert!(keys.contains(&"e=1"), "payloads are base64: {meta}");
            let text = String::from_utf8(BASE64.decode(payload).unwrap()).unwrap();
            let id = keys[0].strip_prefix("i=").unwrap();
            let id = id.splitn(4, '-').nth(3).unwrap().to_owned();
            if keys.contains(&"p=body") {
                found.last_mut().unwrap().2 = text;
            } else {
                found.push((id, text, String::new()));
            }
        }
        found
    }

    fn closed(out: &str) -> Vec<String> {
        out.split("\x1b]99;")
            .skip(1)
            .filter(|s| s.contains("p=close"))
            .map(|s| {
                let id = s.split(':').next().unwrap().strip_prefix("i=").unwrap();
                id.splitn(4, '-').nth(3).unwrap().to_owned()
            })
            .collect()
    }

    #[test]
    fn hidden_usage_queries_do_not_look_like_work() {
        let (mut signals, bytes) = signals();
        let mut model = started();
        signals.sync(&model).unwrap();
        take(&bytes);
        model.begin_usage();
        signals.sync(&model).unwrap();
        end(&mut model, Ok(StopReason::EndTurn));
        signals.sync(&model).unwrap();
        assert_eq!(take(&bytes), "");
    }

    #[test]
    fn the_session_title_names_the_pane_without_control_characters() {
        let (mut signals, bytes) = signals();
        let mut model = started();
        signals.sync(&model).unwrap();
        take(&bytes);
        let title: SessionInfoUpdate =
            serde_json::from_value(serde_json::json!({"title": "Fix\u{7}\u{1b}]2;evil\nlogin"}))
                .unwrap();
        model.apply(BridgeEvent::Update(Box::new(
            SessionUpdate::SessionInfoUpdate(title),
        )));
        signals.sync(&model).unwrap();
        assert_eq!(take(&bytes), "\x1b]2;Fix ]2;evil login\x1b\\");
        signals.clear().unwrap();
        assert_eq!(take(&bytes), "", "nothing to clear while idle");
    }
}
