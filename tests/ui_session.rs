use std::{cell::RefCell, collections::VecDeque, io, rc::Rc, time::Duration};

use acp_tsp::{
    bus::{event_channel, wake_pair},
    event::{BridgeEvent, SessionStartKind, UiCmd},
    launch::AgentLaunch,
    ui::{UiExit, UiOptions, run_with_terminal, run_with_terminal_stop},
};
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, SessionConfigOption, SessionUpdate, TextContent, ToolCallUpdate,
    ToolCallUpdateFields,
};
use tern_sdk::term::Terminal;

#[derive(Default)]
struct Pipes {
    input: VecDeque<Vec<u8>>,
    output: Vec<u8>,
    sent_keys: bool,
    first_input: Option<Vec<u8>>,
    edit_action: Option<Vec<u8>>,
    after_adopt: Option<Vec<u8>>,
    edit_sent: bool,
    edit_output_start: Option<usize>,
    edit_rendered: bool,
    /// Sends Ctrl+D once a write contains this.
    quit_on: Vec<u8>,
    restored: bool,
}

struct Script(Rc<RefCell<Pipes>>);

impl Terminal for Script {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut pipes = self.0.borrow_mut();
        pipes.output.extend_from_slice(bytes);
        if bytes.windows(7).any(|w| w == b"tsp;q;{") {
            pipes.input.push_back(
                format!(
                    "\x1b_tsp;r;{}\x1b\\",
                    serde_json::json!({"r":"hello","v":1,"term":"tern","ver":"0.6.2",
                    "kinds":["col","md","card","editor","status","seg","picker","list"],
                    "features":["flow","edit","undo","send","scroll"],"credits":100,"apc":65536})
                )
                .into_bytes(),
            );
            pipes.input.push_back(b"\x1b[?62;52;c".to_vec());
        } else if bytes
            .windows(b"\"adopt\":true".len())
            .any(|w| w == b"\"adopt\":true")
            && let Some(input) = pipes.after_adopt.take()
        {
            pipes.input.push_back(input);
        } else if bytes.windows(7).any(|w| w == b"tsp;f;{") && !pipes.sent_keys {
            pipes.sent_keys = true;
            if let Some(input) = pipes.first_input.take() {
                pipes.input.push_back(input);
            }
        } else if pipes.edit_action.is_some()
            && bytes
                .windows(b"main.entry.1".len())
                .any(|w| w == b"main.entry.1")
        {
            let action = pipes.edit_action.take().unwrap();
            pipes.edit_sent = true;
            pipes.edit_output_start = Some(pipes.output.len());
            pipes.input.push_back(action);
        } else if pipes.edit_sent
            && !pipes.edit_rendered
            && bytes
                .windows(b"dock.editor.line.ed".len())
                .any(|w| w == b"dock.editor.line.ed")
            && bytes.windows(b"editable".len()).any(|w| w == b"editable")
        {
            pipes.edit_rendered = true;
            pipes
                .input
                .push_back(b"\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x04".to_vec());
        } else if !pipes.quit_on.is_empty()
            && bytes
                .windows(pipes.quit_on.len())
                .any(|w| w == pipes.quit_on)
        {
            pipes.input.push_back(b"\x04".to_vec());
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        let mut pipes = self.0.borrow_mut();
        let Some(chunk) = pipes.input.pop_front() else {
            assert!(timeout.is_some(), "script should end after Ctrl+D");
            return Ok(None);
        };
        let n = chunk.len().min(buf.len());
        buf[..n].copy_from_slice(&chunk[..n]);
        if n < chunk.len() {
            pipes.input.push_front(chunk[n..].to_vec());
        }
        Ok(Some(n))
    }

    fn modes(&mut self, _paste: bool, _kitty: bool) {}
    fn restore(&mut self) -> io::Result<()> {
        self.0.borrow_mut().restored = true;
        Ok(())
    }
}

#[test]
fn scripted_handshake_prompt_and_agent_frame() {
    let pipes = Rc::new(RefCell::new(Pipes {
        first_input: Some(b"hi\r".to_vec()),
        quit_on: b"main.entry.2".to_vec(),
        ..Pipes::default()
    }));
    let (waker, _wake) = wake_pair().unwrap();
    let (tx, rx) = event_channel(waker);
    tx.send(BridgeEvent::Update(Box::new(
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "agent answer",
        )))),
    )));
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    assert_eq!(
        run_with_terminal(
            Script(pipes.clone()),
            UiOptions {
                agent: "Claude".into(),
                cwd: std::env::temp_dir(),
                launch: AgentLaunch {
                    name: "Claude".into(),
                    command: "true".into(),
                    args: vec![],
                    env: Default::default(),
                },
            },
            rx,
            cmd_tx,
            || {}
        )
        .unwrap(),
        UiExit::Quit { resume: None }
    );
    let UiCmd::Prompt(blocks) = cmd_rx.try_recv().unwrap() else {
        panic!("expected prompt")
    };
    assert!(matches!(&blocks[..], [ContentBlock::Text(text)] if text.text == "hi"));
    assert!(matches!(cmd_rx.try_recv().unwrap(), UiCmd::Shutdown));
    let output = String::from_utf8(pipes.borrow().output.clone()).unwrap();
    assert!(
        output.contains("agent answer"),
        "agent message should be in a TSP frame"
    );
    assert!(pipes.borrow().restored);
}

#[test]
fn edit_prompt_action_replaces_the_composer_and_focuses_it() {
    let action = serde_json::json!({"ev": "action", "sf": "s1",
        "id": "main.entry.1", "act": "edit", "mods": []});
    let pipes = Rc::new(RefCell::new(Pipes {
        first_input: Some(b"editable\r".to_vec()),
        edit_action: Some(format!("\x1b_tsp;e;{action}\x1b\\").into_bytes()),
        ..Pipes::default()
    }));
    let (waker, _wake) = wake_pair().unwrap();
    let (_tx, events) = event_channel(waker);
    let (cmds, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    run_with_terminal(
        Script(pipes.clone()),
        UiOptions {
            agent: "Fake".into(),
            cwd: std::env::temp_dir(),
            launch: AgentLaunch {
                name: "Fake".into(),
                command: "true".into(),
                args: vec![],
                env: Default::default(),
            },
        },
        events,
        cmds,
        || {},
    )
    .unwrap();
    let pipes = pipes.borrow();
    assert!(pipes.edit_rendered, "edit action must fill the editor");
    let output = String::from_utf8(pipes.output.clone()).unwrap();
    let after_action = &output[pipes.edit_output_start.expect("edit action was queued")..];
    let ops: Vec<serde_json::Value> = after_action
        .split("\x1b_tsp;f;")
        .skip(1)
        .filter_map(|part| part.split("\x1b\\").next())
        .filter_map(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .flat_map(|frame| frame["ops"].as_array().cloned().unwrap_or_default())
        .collect();
    assert!(
        ops.iter()
            .any(|op| op[0] == "focus" && op[1] == "dock.editor.line.ed"),
        "edit must focus the composer after the action: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| op[0] == "text"
            && op[1] == "dock.editor.line.ed"
            && op[2] == "replace"
            && op[3] == "editable"),
        "edit must replace the editor's text: {ops:?}"
    );
    drop(pipes);
    assert!(matches!(receiver.try_recv().unwrap(), UiCmd::Prompt(_)));
    assert!(matches!(receiver.try_recv().unwrap(), UiCmd::Shutdown));
}

#[test]
fn closed_agent_stays_visible_until_ctrl_d() {
    let pipes = Rc::new(RefCell::new(Pipes {
        first_input: Some(b"\x04".to_vec()),
        ..Pipes::default()
    }));
    let (waker, _wake) = wake_pair().unwrap();
    let (tx, events) = event_channel(waker);
    drop(tx);
    let (cmds, receiver) = tokio::sync::mpsc::unbounded_channel();
    drop(receiver);
    assert_eq!(
        run_with_terminal(
            Script(pipes.clone()),
            UiOptions {
                agent: "Claude".into(),
                cwd: std::env::temp_dir(),
                launch: AgentLaunch {
                    name: "Claude".into(),
                    command: "true".into(),
                    args: vec![],
                    env: Default::default(),
                },
            },
            events,
            cmds,
            || {}
        )
        .unwrap(),
        UiExit::Quit { resume: None }
    );
    let output = String::from_utf8(pipes.borrow().output.clone()).unwrap();
    assert!(output.contains("Agent connection closed"));
    // The exit dialog offers Quit (Ctrl+D) but no Restart: the ACP side is gone.
    assert!(output.contains("\"key\":\"quit\""));
    assert!(!output.contains("\"key\":\"restart\""));
    assert!(pipes.borrow().restored);
}

#[test]
fn answering_a_permission_closes_its_dialog() {
    let pipes = Rc::new(RefCell::new(Pipes {
        first_input: Some(b"1".to_vec()),
        quit_on: br#"["del","layer"]"#.to_vec(),
        ..Pipes::default()
    }));
    let (waker, _wake) = wake_pair().unwrap();
    let (tx, rx) = event_channel(waker);
    tx.send(BridgeEvent::Permission {
        id: 7,
        request: Box::new(RequestPermissionRequest::new(
            "s",
            ToolCallUpdate::new("t", ToolCallUpdateFields::new().title("Run tests")),
            vec![PermissionOption::new(
                "yes",
                "Allow",
                PermissionOptionKind::AllowOnce,
            )],
        )),
    });
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    run_with_terminal(
        Script(pipes.clone()),
        UiOptions {
            agent: "Codex".into(),
            cwd: std::env::temp_dir(),
            launch: AgentLaunch {
                name: "Codex".into(),
                command: "true".into(),
                args: vec![],
                env: Default::default(),
            },
        },
        rx,
        cmd_tx,
        || {},
    )
    .unwrap();
    let UiCmd::Permission { id, outcome } = cmd_rx.try_recv().unwrap() else {
        panic!("expected permission answer")
    };
    assert_eq!(id, 7);
    assert!(
        matches!(outcome, RequestPermissionOutcome::Selected(s) if s.option_id.0.as_ref() == "yes")
    );
    // Reaching Ctrl+D at all means a frame deleted the dialog layer.
    assert!(matches!(cmd_rx.try_recv().unwrap(), UiCmd::Shutdown));
}

#[test]
fn resume_without_an_id_lists_sessions_and_closing_the_picker_starts_a_new_one() {
    let pipes = Rc::new(RefCell::new(Pipes {
        // Escape closes the picker; Ctrl+D then quits.
        first_input: Some(b"\x1b[27u\x04".to_vec()),
        ..Pipes::default()
    }));
    let (waker, _wake) = wake_pair().unwrap();
    let (tx, rx) = event_channel(waker);
    tx.send(BridgeEvent::PickSession);
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    run_with_terminal(
        Script(pipes.clone()),
        UiOptions {
            agent: "Codex".into(),
            cwd: std::env::temp_dir(),
            launch: AgentLaunch {
                name: "Codex".into(),
                command: "true".into(),
                args: vec![],
                env: Default::default(),
            },
        },
        rx,
        cmd_tx,
        || {},
    )
    .unwrap();
    let mut sent = Vec::new();
    while let Ok(cmd) = cmd_rx.try_recv() {
        sent.push(cmd);
    }
    assert!(
        matches!(sent[0], UiCmd::ListSessions { cursor: None }),
        "{sent:?}"
    );
    assert!(matches!(sent[1], UiCmd::NewSession), "{sent:?}");
    assert!(matches!(sent[2], UiCmd::Shutdown), "{sent:?}");
}

fn drive_with_adopt(
    input: Vec<u8>,
    after_adopt: Option<Vec<u8>>,
    event: Option<BridgeEvent>,
    stop: impl Fn() -> io::Result<()>,
) -> (String, Vec<UiCmd>) {
    let pipes = Rc::new(RefCell::new(Pipes {
        first_input: Some(input),
        after_adopt,
        ..Pipes::default()
    }));
    let (waker, _wake) = wake_pair().unwrap();
    let (tx, rx) = event_channel(waker);
    if let Some(event) = event {
        tx.send(event);
    }
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    run_with_terminal_stop(
        Script(pipes.clone()),
        UiOptions {
            agent: "Codex".into(),
            cwd: std::env::temp_dir(),
            launch: AgentLaunch {
                name: "Codex".into(),
                command: "true".into(),
                args: vec![],
                env: Default::default(),
            },
        },
        rx,
        cmd_tx,
        || {},
        stop,
    )
    .unwrap();
    let mut sent = Vec::new();
    while let Ok(cmd) = cmd_rx.try_recv() {
        sent.push(cmd);
    }
    let output = String::from_utf8(pipes.borrow().output.clone()).unwrap();
    (output, sent)
}
fn drive(
    input: Vec<u8>,
    event: Option<BridgeEvent>,
    stop: impl Fn() -> io::Result<()>,
) -> (String, Vec<UiCmd>) {
    drive_with_adopt(input, None, event, stop)
}

fn run_with_input(input: Vec<u8>) -> Vec<UiCmd> {
    drive(input, None, || Ok(())).1
}

fn gone(ids: &[&str]) -> Vec<u8> {
    format!(
        "\x1b_tsp;e;{}\x1b\\",
        serde_json::json!({"ev": "gone", "sf": "s1", "ids": ids})
    )
    .into_bytes()
}

#[test]
fn evicted_transcript_rows_do_not_end_the_session() {
    // Tern drops old settled rows under its retention budget and says so with `gone`.
    let mut input = gone(&["main.entry.1", "main.entry.2"]);
    input.extend_from_slice(b"hi\r\x04");
    let sent = run_with_input(input);
    assert!(
        sent.iter().any(|cmd| matches!(cmd, UiCmd::Prompt(_))),
        "the session kept taking input: {sent:?}"
    );
}

#[test]
fn a_gone_surface_reopens_and_keeps_accepting_prompts() {
    let mut input = gone(&["s1"]);
    input.extend_from_slice(b"hi\r\x04");
    let (output, sent) = drive(input, None, || Ok(()));
    assert_eq!(output.matches("tsp;o;").count(), 2, "{output}");
    assert!(
        sent.iter().any(|cmd| matches!(cmd, UiCmd::Prompt(_))),
        "{sent:?}"
    );
}

#[test]
fn reopening_preserves_a_draft_prompt() {
    let mut input = b"draft".to_vec();
    input.extend_from_slice(&gone(&["s1"]));
    input.extend_from_slice(b"\r\x04");
    let (output, sent) = drive(input, None, || Ok(()));
    assert_eq!(output.matches("tsp;o;").count(), 2, "{output}");
    assert!(
        sent.iter().any(|cmd| matches!(cmd, UiCmd::Prompt(blocks)
            if matches!(&blocks[..], [ContentBlock::Text(text)] if text.text == "draft")
        )),
        "{sent:?}"
    );
}

#[test]
fn an_immediately_gone_reopened_surface_stops_retrying() {
    let mut input = gone(&["s1"]);
    input.extend_from_slice(&gone(&["s1"]));
    input.extend_from_slice(b"hi\r");
    let (output, sent) = drive(input, None, || Ok(()));
    assert_eq!(output.matches("tsp;o;").count(), 2, "{output}");
    assert!(
        !sent.iter().any(|cmd| matches!(cmd, UiCmd::Prompt(_))),
        "{sent:?}"
    );
}

#[test]
fn page_up_reveals_the_previous_user_prompt() {
    let (output, _) = drive(
        b"\x1b[5~\x04".to_vec(),
        Some(BridgeEvent::Update(Box::new(
            SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new("Earlier question"),
            ))),
        ))),
        || Ok(()),
    );
    assert!(
        output.contains("[\"reveal\",\"main.entry.1\",\"start\"]"),
        "{output}"
    );
}

#[test]
fn page_down_scrolls_the_open_command_palette() {
    let (output, _) = drive(b"/\x1b[6~\x1b[27u\x7f\x04".to_vec(), None, || Ok(()));
    assert!(
        output.contains("[\"scroll\",\"layer.complete.0.list\",\"page-down\"]"),
        "{output}"
    );
}

#[test]
fn unsupported_settings_sheet_shows_a_notice() {
    let (output, _) = drive(
        b"/config\r\x04".to_vec(),
        Some(BridgeEvent::SessionStarted {
            session_id: "s".into(),
            kind: SessionStartKind::New,
            cwd: std::env::temp_dir(),
            additional_dirs: vec![],
            modes: None,
            config_options: Some(vec![SessionConfigOption::boolean(
                "setting", "Setting", false,
            )]),
        }),
        || Ok(()),
    );
    assert!(
        output.contains("This Tern version cannot show settings sheets"),
        "{output}"
    );
    assert!(!output.contains("\"k\":\"prefs\""), "{output}");
}

#[test]
fn job_continue_adopts_and_falls_back_when_tern_reports_gone() {
    let mut after_adopt = gone(&["s1"]);
    after_adopt.extend_from_slice(b"hi\r\x04");
    let (output, sent) = drive_with_adopt(b"\x1a".to_vec(), Some(after_adopt), None, || Ok(()));
    assert_eq!(output.matches("tsp;o;").count(), 3, "{output}");
    assert!(output.contains("\"adopt\":true"), "{output}");
    assert!(
        sent.iter().any(|cmd| matches!(cmd, UiCmd::Prompt(_))),
        "{sent:?}"
    );
}
