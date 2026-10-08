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

#[test]
fn edit_fields_and_accept_emit_typed_content() {
    let schema = ElicitationSchema::new()
        .property("name", StringPropertySchema::new(), true)
        .property("count", IntegerPropertySchema::new().minimum(1), true);
    let (wake, _reader) = wake_pair().unwrap();
    let (tx, events) = event_channel(wake);
    tx.send(BridgeEvent::Elicitation {
        id: 9,
        request: Box::new(CreateElicitationRequest::new(
            ElicitationFormMode::new(ElicitationSessionScope::new("s"), schema),
            "Details?",
        )),
    });
    let pipes = Rc::new(RefCell::new(Pipes::default()));
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
    let UiCmd::Elicitation { id, response } = receiver.try_recv().unwrap() else {
        panic!("elicitation response")
    };
    assert_eq!(id, 9);
    let ElicitationAction::Accept(accept) = response.action else {
        panic!("accepted")
    };
    let values = accept.content.unwrap();
    assert_eq!(
        values["name"],
        ElicitationContentValue::String("Ada".into())
    );
    assert_eq!(values["count"], ElicitationContentValue::Integer(3));
    assert!(matches!(receiver.try_recv().unwrap(), UiCmd::Shutdown));
    assert!(pipes.borrow().quit);
}
