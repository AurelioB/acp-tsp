use std::{cell::RefCell, collections::VecDeque, io, rc::Rc, time::Duration};

use acp_tsp::{
    bus::{event_channel, wake_pair},
    event::{BridgeEvent, SessionStartKind, UiCmd},
    launch::AgentLaunch,
    model::Model,
    ui::{
        UiOptions,
        palette::{Kind, Palette},
        prefs, run_with_terminal,
    },
};
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthMethodTerminal, AvailableCommand, AvailableCommandsUpdate, SessionConfigOption,
    SessionConfigOptionCategory, SessionUpdate,
};
use serde_json::{Value, json};
use tern_sdk::{reconcile::Doc, term::Terminal, ui::View};

#[derive(Default)]
struct Pipes {
    incoming: VecDeque<Vec<u8>>,
    outgoing: Vec<u8>,
    script: Option<Vec<u8>>,
    delivered: bool,
    stalled: usize,
}
struct Script(Rc<RefCell<Pipes>>);
impl Terminal for Script {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut pipes = self.0.borrow_mut();
        pipes.outgoing.extend_from_slice(bytes);
        if bytes.windows(7).any(|part| part == b"tsp;q;{") {
            pipes.incoming.push_back(
                format!(
                    "\x1b_tsp;r;{}\x1b\\",
                    json!({
                        "r":"hello", "v":1, "term":"tern", "ver":"0.6.2", "credits":100,
                        "apc":65536, "features":["flow","edit","undo","send"],
                        "kinds":["col","md","card","editor","status","seg","picker","prefs"]
                    })
                )
                .into_bytes(),
            );
            pipes.incoming.push_back(b"\x1b[?62;52;c".to_vec());
        } else if bytes.windows(7).any(|part| part == b"tsp;f;{") && !pipes.delivered {
            pipes.delivered = true;
            if let Some(script) = pipes.script.take() {
                pipes.incoming.push_back(script);
            }
        }
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        let mut pipes = self.0.borrow_mut();
        let Some(bytes) = pipes.incoming.pop_front() else {
            if timeout.is_some() {
                pipes.stalled += 1;
                assert!(
                    pipes.stalled < 20,
                    "script did not finish; output: {}",
                    String::from_utf8_lossy(&pipes.outgoing)
                );
                return Ok(None);
            }
            panic!(
                "missing scripted input; output: {}",
                String::from_utf8_lossy(&pipes.outgoing)
            );
        };
        let count = bytes.len().min(buf.len());
        pipes.stalled = 0;
        buf[..count].copy_from_slice(&bytes[..count]);
        if count < bytes.len() {
            pipes.incoming.push_front(bytes[count..].to_vec());
        }
        Ok(Some(count))
    }
    fn modes(&mut self, _: bool, _: bool) {}
}

fn launch() -> AgentLaunch {
    AgentLaunch {
        name: "Test".into(),
        command: "sh".into(),
        args: vec!["-c".into(), "test \"$AUTH_FLAG\" = yes".into()],
        env: [("AUTH_FLAG".into(), "no".into())].into(),
    }
}

fn drive(script: &[u8], event: Option<BridgeEvent>) -> (String, Vec<UiCmd>) {
    let pipes = Rc::new(RefCell::new(Pipes {
        script: Some(script.into()),
        ..Pipes::default()
    }));
    let (waker, _) = wake_pair().unwrap();
    let (events_tx, events) = event_channel(waker);
    if let Some(event) = event {
        events_tx.send(event);
    }
    let (cmds, mut commands) = tokio::sync::mpsc::unbounded_channel();
    run_with_terminal(
        Script(pipes.clone()),
        UiOptions {
            agent: "Test".into(),
            cwd: std::env::temp_dir(),
            launch: launch(),
        },
        events,
        cmds,
    )
    .unwrap();
    let output = String::from_utf8(pipes.borrow().outgoing.clone()).unwrap();
    let mut collected = Vec::new();
    while let Ok(cmd) = commands.try_recv() {
        collected.push(cmd);
    }
    (output, collected)
}

#[test]
fn slash_opens_picker_then_enter_sends_client_new_session() {
    let (output, cmds) = drive(b"/\r\x04", None);
    assert!(
        output.contains("\"k\":\"picker\""),
        "slash must show a native picker"
    );
    assert!(cmds.iter().any(|cmd| matches!(cmd, UiCmd::NewSession)));
    assert!(matches!(cmds.last(), Some(UiCmd::Shutdown)));
}

#[test]
fn terminal_auth_suspends_runs_command_resumes_then_retries() {
    let mut method = AuthMethodTerminal::new("terminal", "Terminal login");
    method.env.insert("AUTH_FLAG".into(), "yes".into());
    let (output, cmds) = drive(
        b"\r\x04",
        Some(BridgeEvent::AuthRequired {
            methods: vec![AuthMethod::Terminal(method)],
            message: None,
        }),
    );
    assert!(
        output.contains("suspend"),
        "TSP surface must suspend before interactive child"
    );
    assert!(
        output.contains("resume"),
        "TSP surface must resume after child exits"
    );
    assert!(cmds.iter().any(|cmd| matches!(cmd, UiCmd::RetrySession)));
    assert!(
        !cmds.iter().any(|cmd| matches!(cmd, UiCmd::Authenticate(_))),
        "terminal methods must not call ACP authenticate"
    );
}

#[test]
fn failed_terminal_login_restores_surface_without_retry() {
    let (output, cmds) = drive(
        b"\r\x04",
        Some(BridgeEvent::AuthRequired {
            methods: vec![AuthMethod::Terminal(AuthMethodTerminal::new(
                "terminal",
                "Terminal login",
            ))],
            message: None,
        }),
    );
    assert!(output.contains("suspend") && output.contains("resume"));
    assert!(output.contains("Authentication failed"));
    assert!(!cmds.iter().any(|cmd| matches!(cmd, UiCmd::RetrySession)));
}

fn layer(view: View<acp_tsp::ui::Msg>) -> Value {
    let doc = Doc::from_view(view).unwrap();
    serde_json::to_value(doc.regions()[2].as_ref().unwrap().wire()).unwrap()
}

#[test]
fn picker_snapshot() {
    let model = Model::default();
    insta::assert_json_snapshot!(
        "ui_session_palette",
        layer(View::new().layer(Palette::new(Kind::Commands).render(&model)))
    );
}

#[test]
fn prefs_snapshot() {
    let mut model = Model::default();
    model.apply(BridgeEvent::SessionStarted {
        session_id: "s".into(),
        kind: SessionStartKind::New,
        cwd: "/tmp/project".into(),
        additional_dirs: vec![],
        modes: None,
        config_options: Some(vec![
            SessionConfigOption::select(
                "mode",
                "Mode",
                "plan",
                vec![
                    agent_client_protocol::schema::v1::SessionConfigSelectOption::new(
                        "plan", "Plan",
                    ),
                    agent_client_protocol::schema::v1::SessionConfigSelectOption::new(
                        "build", "Build",
                    ),
                ],
            )
            .category(SessionConfigOptionCategory::Mode),
            SessionConfigOption::boolean("safe", "Safe", true),
        ]),
    });
    insta::assert_json_snapshot!(
        "ui_session_prefs",
        layer(View::new().layer(prefs::render(&model)))
    );
}

#[test]
fn palette_ranks_command_names_above_description_matches() {
    // Agent commands are listed first, but a name-prefix match (`/new`) must outrank a
    // name-substring (`renew-token`) or description match.
    let (_, cmds) = drive(
        b"/new\r\x1b[27u\x04",
        Some(BridgeEvent::Update(Box::new(
            SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
                AvailableCommand::new("renew-token", "Refresh credentials"),
                AvailableCommand::new("review", "Review the new changes"),
            ])),
        ))),
    );
    assert!(
        cmds.iter().any(|cmd| matches!(cmd, UiCmd::NewSession)),
        "Enter must pick /new, got {cmds:?}"
    );
}
