//! Pane-level signals beside TSP: the title (OSC 2), working/waiting progress (OSC 9;4) and
//! attention notifications (OSC 99). Tern shows them on tabs and pane headers, as the
//! progress edge and agent working/idle/waiting state, and in the attention inbox.
//!
//! They share the pty with TSP frames; every write here is one whole sequence issued
//! between session calls on the UI thread, so it never splits a frame.
use std::{cell::RefCell, io, rc::Rc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use tern_sdk::term::Terminal;

use super::paste::{self, PasteFilter, PastedImage};
use crate::model::{Model, Turn, TurnOutcome};

/// Pastes taken out of the input for the UI: images and errors to show.
#[derive(Default)]
pub struct PasteInbox {
    pub images: Vec<PastedImage>,
    pub errors: Vec<String>,
}

struct Shared {
    term: Box<dyn Terminal>,
    paste: PasteFilter,
    /// Filtered input that did not fit the caller's buffer.
    spill: Vec<u8>,
    inbox: Rc<RefCell<PasteInbox>>,
    paste_enabled: bool,
}

/// The session's terminal, shared so pane signals can be written between frames. Its input
/// passes through the enhanced-paste filter.
pub struct SharedTerm(Rc<RefCell<Shared>>);

impl SharedTerm {
    pub fn new(term: impl Terminal + 'static, paste_limit: usize) -> Self {
        Self(Rc::new(RefCell::new(Shared {
            term: Box::new(term),
            paste: PasteFilter::new(paste_limit),
            spill: Vec::new(),
            inbox: Rc::default(),
            paste_enabled: false,
        })))
    }

    pub(super) fn handle(&self) -> Self {
        Self(self.0.clone())
    }

    pub fn paste_inbox(&self) -> Rc<RefCell<PasteInbox>> {
        self.0.borrow().inbox.clone()
    }

    /// Asks Tern for enhanced paste (undone by `restore`).
    pub fn enable_paste(&self) -> io::Result<()> {
        let mut shared = self.0.borrow_mut();
        shared.term.write(paste::ENABLE)?;
        shared.paste_enabled = true;
        Ok(())
    }

    /// Gives pastes back to whatever reads the pty next (the shell, while suspended).
    pub fn disable_paste(&self) -> io::Result<()> {
        let mut shared = self.0.borrow_mut();
        if std::mem::take(&mut shared.paste_enabled) {
            shared.term.write(paste::DISABLE)?;
        }
        Ok(())
    }
}

impl Terminal for SharedTerm {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.borrow_mut().term.write(bytes)
    }
    fn read(&mut self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        let mut shared = self.0.borrow_mut();
        loop {
            if !shared.spill.is_empty() {
                let n = buf.len().min(shared.spill.len());
                buf[..n].copy_from_slice(&shared.spill[..n]);
                shared.spill.drain(..n);
                return Ok(Some(n));
            }
            // A possible packet start is held back; if nothing follows quickly it was a key.
            const HOLD: Duration = Duration::from_millis(30);
            let holding = shared.paste.holding();
            let wait = if holding {
                Some(timeout.map_or(HOLD, |timeout| timeout.min(HOLD)))
            } else {
                timeout
            };
            let Some(n) = shared.term.read(buf, wait)? else {
                if holding {
                    shared.spill = shared.paste.flush_held();
                    continue;
                }
                return Ok(None);
            };
            if n == 0 {
                return Ok(Some(0));
            }
            let filtered = shared.paste.feed(&buf[..n]);
            for reply in &filtered.replies {
                shared.term.write(reply)?;
            }
            let mut input = filtered.input;
            if !filtered.images.is_empty() || !filtered.errors.is_empty() {
                let mut inbox = shared.inbox.borrow_mut();
                inbox.images.extend(filtered.images);
                inbox.errors.extend(filtered.errors);
                // Wake the session loop so the UI takes the paste now.
                input.extend_from_slice(crate::bus::WAKE_EVENT);
            }
            shared.spill = input;
            if shared.spill.is_empty() && timeout.is_some() && !shared.paste.holding() {
                // Only packets arrived: to the caller this read timed out.
                return Ok(None);
            }
        }
    }
    fn restore(&mut self) -> io::Result<()> {
        self.disable_paste()?;
        self.0.borrow_mut().term.restore()
    }
    fn modes(&mut self, paste: bool, kitty: bool) {
        self.0.borrow_mut().term.modes(paste, kitty);
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
    /// The last state written; `None` until the first sync. Tern treats a running command as
    /// busy until it reports progress, so idle must be said explicitly too.
    progress: Option<Progress>,
    dialog: Option<u64>,
    /// The last OSC 7501 report written.
    status: Option<String>,
    finished_turns: u64,
    exited: bool,
}

impl PaneSignals {
    pub fn new(term: &SharedTerm) -> Self {
        Self {
            out: term.handle(),
            title: None,
            progress: None,
            dialog: None,
            finished_turns: 0,
            exited: false,
            status: None,
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
        if Some(progress) != self.progress {
            self.out
                .write(format!("\x1b]9;4;{};\x1b\\", progress as u8).as_bytes())?;
            self.progress = Some(progress);
        }
        let status = status_report(model);
        if self.status.as_ref() != Some(&status) {
            self.out.write(status.as_bytes())?;
            self.status = Some(status);
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

    /// Leaves no progress or status record behind when the program quits.
    pub fn clear(&mut self) -> io::Result<()> {
        if self
            .progress
            .is_some_and(|progress| progress != Progress::Clear)
        {
            self.out.write(b"\x1b]9;4;0;\x1b\\")?;
            self.progress = Some(Progress::Clear);
        }
        if self.status.take().is_some() {
            self.out.write(b"\x1b]7501;state=clear\x1b\\")?;
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

/// The OSC 7501 (Program Status Protocol, revision 0.3) report for the root record: one state
/// for the whole session, with fixed messages only, so no prompt or agent text leaves the pane.
/// Sent alongside OSC 9;4, which the protocol allows.
pub fn status_report(model: &Model) -> String {
    let (state, kind, msg) = if model.exited.is_some() {
        ("error", None, Some("Agent exited"))
    } else if model.oldest_permission().is_some() {
        ("blocked", Some("permission"), Some("Permission needed"))
    } else if let Some(request) = model.oldest_elicitation() {
        if matches!(
            request.request.mode,
            agent_client_protocol::schema::v1::ElicitationMode::Url(_)
        ) {
            ("blocked", Some("auth"), Some("Sign-in required"))
        } else {
            ("blocked", Some("question"), Some("Question waiting"))
        }
    } else if model.auth.is_some() {
        ("blocked", Some("auth"), Some("Sign-in required"))
    } else if matches!(model.turn, Turn::Running { .. }) && !model.usage_report.fetching {
        ("working", None, None)
    } else {
        match model.last_outcome {
            Some(TurnOutcome::Done) => ("done", None, Some("Turn finished")),
            Some(TurnOutcome::Stopped(_)) => ("done", None, Some("Turn stopped early")),
            Some(TurnOutcome::Failed) => ("error", None, Some("Turn failed")),
            // Cancelling returns to idle, as the protocol asks.
            Some(TurnOutcome::Cancelled) | None => ("idle", None, None),
        }
    };
    let mut report = format!("\x1b]7501;state={state}");
    if let Some(kind) = kind {
        report.push_str(&format!(":kind={kind}"));
    }
    report.push_str(&format!(":app={}", status_app(model)));
    if let Some(msg) = msg {
        report.push_str(&format!(":msg={}", BASE64.encode(msg)));
    }
    report.push_str("\x1b\\");
    report
}

/// A stable `app` name in the protocol's character set (`[A-Za-z0-9_.+-]{1,32}`).
fn status_app(model: &Model) -> String {
    let name = model
        .agent
        .as_ref()
        .map_or(model.launch_name.as_str(), |agent| agent.name.as_str());
    match name {
        crate::quirks::CODEX_ACP => return "codex".into(),
        crate::quirks::CLAUDE_ACP => return "claude-code".into(),
        _ => {}
    }
    let name: String = name
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'))
        .take(32)
        .collect();
    if name.is_empty() {
        "acp-tsp".into()
    } else {
        name
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
        let term = SharedTerm::new(Capture(bytes.clone()), 1024);
        (PaneSignals::new(&term), bytes)
    }

    /// Everything written but OSC 7501 status reports (checked by `statuses`).
    fn take(bytes: &Rc<RefCell<Vec<u8>>>) -> String {
        let (other, _) = split_status(&raw(bytes));
        other
    }

    fn raw(bytes: &Rc<RefCell<Vec<u8>>>) -> String {
        String::from_utf8(std::mem::take(&mut *bytes.borrow_mut())).unwrap()
    }

    /// Splits written output into the rest and the OSC 7501 report bodies.
    fn split_status(out: &str) -> (String, Vec<String>) {
        let mut other = String::new();
        let mut reports = Vec::new();
        let mut rest = out;
        while let Some(start) = rest.find("\x1b]7501;") {
            other.push_str(&rest[..start]);
            let body = &rest[start + "\x1b]7501;".len()..];
            let end = body.find("\x1b\\").expect("terminated with ST");
            reports.push(body[..end].to_owned());
            rest = &body[end + 2..];
        }
        other.push_str(rest);
        (other, reports)
    }

    /// The OSC 7501 reports written since the last call, `msg` decoded for reading.
    fn statuses(bytes: &Rc<RefCell<Vec<u8>>>) -> Vec<String> {
        split_status(&raw(bytes))
            .1
            .into_iter()
            .map(|body| {
                body.split(':')
                    .map(|pair| match pair.split_once('=') {
                        Some(("msg", value)) => format!(
                            "msg={}",
                            String::from_utf8(BASE64.decode(value).unwrap()).unwrap()
                        ),
                        _ => pair.to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join(":")
            })
            .collect()
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
            "\x1b]2;Codex\x1b\\\x1b]9;4;0;\x1b\\",
            "untitled: the agent names the pane; idle is reported, not left to Tern's \"a command runs\" default"
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

    #[test]
    fn program_status_follows_the_session_lifecycle() {
        use agent_client_protocol::schema::v1::{
            CreateElicitationRequest, ElicitationFormMode, ElicitationSchema,
            ElicitationSessionScope,
        };
        let (mut signals, bytes) = signals();
        let mut model = started();
        signals.sync(&model).unwrap();
        assert_eq!(statuses(&bytes), ["state=idle:app=Codex"]);
        signals.sync(&model).unwrap();
        assert!(statuses(&bytes).is_empty(), "only changes are reported");

        model.submitted("hi".into());
        signals.sync(&model).unwrap();
        assert_eq!(statuses(&bytes), ["state=working:app=Codex"]);

        permission(&mut model, 7);
        signals.sync(&model).unwrap();
        assert_eq!(
            statuses(&bytes),
            ["state=blocked:kind=permission:app=Codex:msg=Permission needed"]
        );
        model.apply(BridgeEvent::Resolved(7));
        model.apply(BridgeEvent::Elicitation {
            id: 8,
            request: Box::new(CreateElicitationRequest::new(
                ElicitationFormMode::new(
                    ElicitationSessionScope::new("s1"),
                    ElicitationSchema::new(),
                ),
                "Pick one",
            )),
        });
        signals.sync(&model).unwrap();
        assert_eq!(
            statuses(&bytes),
            ["state=blocked:kind=question:app=Codex:msg=Question waiting"],
            "a fixed message: the agent's question text stays in the pane"
        );
        model.apply(BridgeEvent::Resolved(8));
        signals.sync(&model).unwrap();
        assert_eq!(statuses(&bytes), ["state=working:app=Codex"]);

        end(&mut model, Ok(StopReason::EndTurn));
        signals.sync(&model).unwrap();
        assert_eq!(statuses(&bytes), ["state=done:app=Codex:msg=Turn finished"]);

        // A hidden usage query is no new work: the result stays unseen-done.
        model.begin_usage();
        signals.sync(&model).unwrap();
        assert!(statuses(&bytes).is_empty());
        end(&mut model, Ok(StopReason::EndTurn));

        model.submitted("again".into());
        model.cancel();
        end(&mut model, Ok(StopReason::Cancelled));
        signals.sync(&model).unwrap();
        assert_eq!(
            statuses(&bytes),
            ["state=idle:app=Codex"],
            "cancelling reports idle"
        );

        model.submitted("third".into());
        end(&mut model, Err("boom".into()));
        signals.sync(&model).unwrap();
        assert_eq!(statuses(&bytes), ["state=error:app=Codex:msg=Turn failed"]);

        model.apply(BridgeEvent::AgentExited {
            reason: "gone".into(),
        });
        signals.sync(&model).unwrap();
        assert_eq!(statuses(&bytes), ["state=error:app=Codex:msg=Agent exited"]);

        signals.clear().unwrap();
        assert_eq!(
            statuses(&bytes),
            ["state=clear"],
            "quitting leaves no record"
        );
    }

    #[test]
    fn known_adapters_report_their_program_name() {
        use agent_client_protocol::schema::{
            ProtocolVersion,
            v1::{Implementation, InitializeResponse},
        };
        for (name, app) in [
            ("@agentclientprotocol/codex-acp", "codex"),
            ("@agentclientprotocol/claude-agent-acp", "claude-code"),
            ("@scope/some agent!", "someagent"),
        ] {
            let mut model = Model::default();
            model.apply(BridgeEvent::Initialized(Box::new(
                InitializeResponse::new(ProtocolVersion::V1)
                    .agent_info(Implementation::new(name, "1")),
            )));
            assert!(
                status_report(&model).contains(&format!(":app={app}\x1b")),
                "{name}: {}",
                status_report(&model)
            );
        }
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
