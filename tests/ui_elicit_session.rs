use std::{cell::RefCell, collections::VecDeque, io, rc::Rc, time::Duration};

use acp_tsp::{
    bus::{event_channel, wake_pair},
    event::{BridgeEvent, UiCmd},
    launch::AgentLaunch,
    ui::{UiOptions, run_with_terminal},
};
use agent_client_protocol::schema::v1::{
    CreateElicitationRequest, ElicitationAction, ElicitationContentValue, ElicitationFormMode,
    ElicitationSchema, ElicitationSessionScope, IntegerPropertySchema, StringPropertySchema,
};
use tern_sdk::term::Terminal;

#[derive(Default)]
struct Pipes {
    input: VecDeque<Vec<u8>>,
    output: Vec<u8>,
    sent: bool,
    quit: bool,
    /// Input to deliver once the dialog is drawn; `None` fills in the default form.
    script: Option<Vec<Vec<u8>>>,
}
struct Script(Rc<RefCell<Pipes>>);
impl Terminal for Script {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut pipes = self.0.borrow_mut();
        pipes.output.extend_from_slice(bytes);
        if bytes.windows(7).any(|w| w == b"tsp;q;{") {
            pipes.input.push_back(format!("\x1b_tsp;r;{}\x1b\\", serde_json::json!({
                "r":"hello", "v":1,"term":"tern","ver":"0.6.2", "kinds":["col","card","el","input","editor","md"],
                "features":["flow","edit","undo","send"],"credits":100,"apc":65536})).into_bytes());
            pipes.input.push_back(b"\x1b[?62;52;c".to_vec());
        } else if bytes.windows(7).any(|w| w == b"tsp;f;{") && !pipes.sent {
            pipes.sent = true;
            if let Some(script) = pipes.script.take() {
                pipes.input.extend(script);
                return Ok(());
            }
            for (name, text) in [("name", "Ada"), ("count", "3")] {
                let id = format!("layer.elicitation.form.{name}.field.{name}");
                pipes.input.push_back(format!("\x1b_tsp;e;{}\x1b\\", serde_json::json!({
                    "ev":"edit", "sf":"s1","id":id,"from":0,"to":0,"text":text,"cursor":text.len(),"len":0
                })).into_bytes());
            }
            pipes.input.push_back(b"\r".to_vec());
        } else if pipes.sent
            && !pipes.quit
            && bytes
                .windows(br#"["del","layer"]"#.len())
                .any(|w| w == br#"["del","layer"]"#)
        {
            pipes.quit = true;
            pipes.input.push_back(b"\x04".to_vec());
        }
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        let mut pipes = self.0.borrow_mut();
        let Some(bytes) = pipes.input.pop_front() else {
            assert!(timeout.is_some(), "script ended before dialog closed");
            return Ok(None);
        };
        let size = buf.len().min(bytes.len());
        buf[..size].copy_from_slice(&bytes[..size]);
        if size < bytes.len() {
            pipes.input.push_front(bytes[size..].to_vec());
        }
        Ok(Some(size))
    }
    fn modes(&mut self, _: bool, _: bool) {}
    fn restore(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn details() -> CreateElicitationRequest {
    let schema = ElicitationSchema::new()
        .property("name", StringPropertySchema::new(), true)
        .property("count", IntegerPropertySchema::new().minimum(1), true);
    CreateElicitationRequest::new(
        ElicitationFormMode::new(ElicitationSessionScope::new("s"), schema),
        "Details?",
    )
}

fn run(
    request: CreateElicitationRequest,
    script: Option<Vec<Vec<u8>>>,
) -> (Rc<RefCell<Pipes>>, Vec<UiCmd>) {
    let (wake, _reader) = wake_pair().unwrap();
    let (tx, events) = event_channel(wake);
    tx.send(BridgeEvent::Elicitation {
        id: 9,
        request: Box::new(request),
    });
    let pipes = Rc::new(RefCell::new(Pipes {
        script,
        ..Pipes::default()
    }));
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
    let mut sent = Vec::new();
    while let Ok(cmd) = receiver.try_recv() {
        sent.push(cmd);
    }
    (pipes, sent)
}

fn action(response: &UiCmd) -> &ElicitationAction {
    let UiCmd::Elicitation { id, response } = response else {
        panic!("elicitation response, got {response:?}")
    };
    assert_eq!(*id, 9);
    &response.action
}

#[test]
fn escape_cancels_the_question() {
    let (pipes, sent) = run(details(), Some(vec![b"\x1b[27u".to_vec()]));
    assert!(matches!(action(&sent[0]), ElicitationAction::Cancel));
    assert!(pipes.borrow().quit, "the dialog must close");
}

#[test]
fn decline_button_declines_the_question() {
    let click = serde_json::json!({"ev": "action", "sf": "s1",
        "id": "layer.elicitation.actions.decline", "act": "click", "mods": []});
    let (pipes, sent) = run(
        details(),
        Some(vec![format!("\x1b_tsp;e;{click}\x1b\\").into_bytes()]),
    );
    assert!(matches!(action(&sent[0]), ElicitationAction::Decline));
    assert!(pipes.borrow().quit, "the dialog must close");
}

#[test]
fn edit_fields_and_accept_emit_typed_content() {
    let (pipes, sent) = run(details(), None);
    let ElicitationAction::Accept(accept) = action(&sent[0]) else {
        panic!("accepted")
    };
    let values = accept.content.as_ref().unwrap();
    assert_eq!(
        values["name"],
        ElicitationContentValue::String("Ada".into())
    );
    assert_eq!(values["count"], ElicitationContentValue::Integer(3));
    assert!(matches!(sent[1], UiCmd::Shutdown));
    assert!(pipes.borrow().quit);
}

#[test]
fn codex_question_keys_pick_from_the_clicked_row_and_type_into_the_note() {
    // A real Codex question: options, then a note field holding the caret.
    let request: CreateElicitationRequest =
        serde_json::from_str(include_str!("fixtures/codex/question.json")).unwrap();
    let click = serde_json::json!({"ev": "action", "sf": "s1",
        "id": "layer.elicitation.form.readme_spelling.options.option.1", "act": "click", "mods": []});
    let (pipes, sent) = run(
        request,
        Some(vec![
            format!("\x1b_tsp;e;{click}\x1b\\").into_bytes(),
            // Down from the clicked "British" row lands on "None of the above".
            b"\x1b[B".to_vec(),
            // Terminal keys, Space included, go to the note rather than the options.
            b"Canadian eh".to_vec(),
            b"\r".to_vec(),
        ]),
    );
    let ElicitationAction::Accept(accept) = action(&sent[0]) else {
        panic!("accepted, got {:?}", sent[0])
    };
    let values = accept.content.as_ref().unwrap();
    assert_eq!(
        values["readme_spelling"],
        ElicitationContentValue::String("None of the above".into())
    );
    assert_eq!(
        values["readme_spelling_note"],
        ElicitationContentValue::String("Canadian eh".into())
    );
    assert!(pipes.borrow().quit, "the dialog must close");
}

fn form(properties: serde_json::Value) -> CreateElicitationRequest {
    serde_json::from_value(serde_json::json!({
        "sessionId": "s", "mode": "form", "message": "Details?",
        "requestedSchema": {"type": "object", "properties": properties}
    }))
    .unwrap()
}

fn tsp(event: serde_json::Value) -> Vec<u8> {
    format!("\x1b_tsp;e;{event}\x1b\\").into_bytes()
}

#[test]
fn terminal_keys_edit_text_around_wide_characters_and_paste() {
    let request = form(serde_json::json!({
        "a_text": {"type": "string", "default": "😀b"},
        "b_count": {"type": "integer", "default": 3}
    }));
    let count_focus = tsp(serde_json::json!({"ev": "focus", "sf": "s1",
        "id": "layer.elicitation.form.b_count.field.b_count"}));
    let (_, sent) = run(
        request,
        Some(vec![
            // The first text field holds the caret, at the end of its default.
            b"\x1b[D".to_vec(),                // Left: before "b"
            b"a".to_vec(),                     // "😀ab"
            b"\x1b[200~x y\x1b[201~".to_vec(), // paste: "😀ax yb"
            b"\x1b[F\x7f".to_vec(),            // End, Backspace: "😀ax y"
            b"\x1b[H\x1b[C\x1b[3~".to_vec(),   // Home, Right, Delete: "😀x y"
            count_focus,
            b"0".to_vec(), // the numeric default edits as text: "30"
            b"\r".to_vec(),
        ]),
    );
    let ElicitationAction::Accept(accept) = action(&sent[0]) else {
        panic!("accepted, got {:?}", sent[0])
    };
    let values = accept.content.as_ref().unwrap();
    assert_eq!(
        values["a_text"],
        ElicitationContentValue::String("😀x y".into())
    );
    assert_eq!(values["b_count"], ElicitationContentValue::Integer(30));
}

#[test]
fn arrows_in_a_multi_choice_list_take_space_from_the_note() {
    let request = form(serde_json::json!({
        "tags": {"type": "array", "items": {"type": "string", "enum": ["one", "two", "three"]}},
        "note": {"type": "string"}
    }));
    let note_focus = tsp(serde_json::json!({"ev": "focus", "sf": "s1",
        "id": "layer.elicitation.form.note.field.note"}));
    let (_, sent) = run(
        request,
        Some(vec![
            b"\x1b[B".to_vec(), // Down to "two": the list takes the keys
            b" ".to_vec(),      // Space toggles "two"
            note_focus,         // a click on the note gives it the keys back
            b"a b".to_vec(),
            b"\r".to_vec(),
        ]),
    );
    let ElicitationAction::Accept(accept) = action(&sent[0]) else {
        panic!("accepted, got {:?}", sent[0])
    };
    let values = accept.content.as_ref().unwrap();
    assert_eq!(
        values["tags"],
        ElicitationContentValue::StringArray(vec!["two".into()])
    );
    assert_eq!(
        values["note"],
        ElicitationContentValue::String("a b".into())
    );
}
