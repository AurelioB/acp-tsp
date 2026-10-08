//! Pane-level signals beside TSP: the title (OSC 2), working/waiting progress (OSC 9;4) and
//! attention notifications (OSC 99). Tern shows them on tabs and pane headers, as the
//! progress edge and agent working/idle/waiting state, and in the attention inbox.
//!
//! They share the pty with TSP frames; every write here is one whole sequence issued
//! between session calls on the UI thread, so it never splits a frame.
use std::{cell::RefCell, io, rc::Rc, time::Duration};

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
        if dialog.is_some() && dialog != self.dialog {
            let what = if model.oldest_permission().is_some() {
                "Permission needed"
            } else {
                "Question waiting"
            };
            self.notify(&agent, what)?;
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
                self.notify(&agent, &body)?;
            }
        }
        let exited = model.exited.is_some();
        if exited && !self.exited {
            self.notify(&agent, "Agent exited")?;
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

    fn notify(&mut self, agent: &str, body: &str) -> io::Result<()> {
        // Basic OSC 99 (kitty notifications): one line, no metadata.
        self.out
            .write(format!("\x1b]99;;{}: {}\x1b\\", agent, clean(body)).as_bytes())
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
        assert_eq!(
            take(&bytes),
            "\x1b]9;4;4;\x1b\\\x1b]99;;Codex: Permission needed\x1b\\"
        );
        signals.sync(&model).unwrap();
        assert_eq!(take(&bytes), "", "one notification per request");

        model.apply(BridgeEvent::Resolved(7));
        signals.sync(&model).unwrap();
        assert_eq!(take(&bytes), "\x1b]9;4;3;\x1b\\");

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
        assert!(take(&bytes).ends_with("\x1b]99;;Codex: Stopped with error\x1b\\"));
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
