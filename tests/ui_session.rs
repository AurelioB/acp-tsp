use std::{cell::RefCell, collections::VecDeque, io, rc::Rc, time::Duration};

use acp_tsp::{
    bus::{event_channel, wake_pair},
    event::{BridgeEvent, UiCmd},
    ui::{UiExit, UiOptions, run_with_terminal},
};
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, SessionUpdate, TextContent, ToolCallUpdate, ToolCallUpdateFields,
};
use tern_sdk::term::Terminal;

#[derive(Default)]
struct Pipes {
    input: VecDeque<Vec<u8>>,
    output: Vec<u8>,
    sent_keys: bool,
    first_input: Option<Vec<u8>>,
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
                    "kinds":["col","md","card","editor","status","seg"],
                    "features":["flow","edit","undo","send"],"credits":100,"apc":65536})
                )
                .into_bytes(),
            );
            pipes.input.push_back(b"\x1b[?62;52;c".to_vec());
        } else if bytes.windows(7).any(|w| w == b"tsp;f;{") && !pipes.sent_keys {
            pipes.sent_keys = true;
            if let Some(input) = pipes.first_input.take() {
                pipes.input.push_back(input);
            }
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
            },
            rx,
            cmd_tx
        )
        .unwrap(),
        UiExit::Quit
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
            },
            events,
            cmds
        )
        .unwrap(),
        UiExit::Quit
    );
    let output = String::from_utf8(pipes.borrow().output.clone()).unwrap();
    assert!(output.contains("Agent connection closed"));
    assert!(output.contains("Press Ctrl+D to quit"));
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
        },
        rx,
        cmd_tx,
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
