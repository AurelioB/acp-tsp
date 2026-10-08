//! ACP client connection and turn driver.
mod handlers;
#[cfg(feature = "unstable_mcp_over_acp")]
mod mcp;
mod pending;
mod session;

use parking_lot::Mutex;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use agent_client_protocol::schema::ProtocolVersion;
#[cfg(feature = "unstable_session_compaction")]
use agent_client_protocol::schema::v1::CompactionCapabilities;
#[cfg(feature = "unstable_session_notices")]
use agent_client_protocol::schema::v1::NoticeCapabilities;
#[cfg(feature = "unstable_plan_operations")]
use agent_client_protocol::schema::v1::PlanCapabilities;
#[cfg(feature = "unstable_subagents")]
use agent_client_protocol::schema::v1::SubagentCapabilities;
use agent_client_protocol::schema::v1::{
    AgentNotification, AuthCapabilities, BooleanConfigOptionCapabilities, ClientCapabilities,
    ClientSessionCapabilities, CompleteElicitationNotification, CreateElicitationRequest,
    CreateTerminalRequest, ElicitationCapabilities, ElicitationFormCapabilities,
    ElicitationUrlCapabilities, FileSystemCapabilities, Implementation, InitializeRequest,
    KillTerminalRequest, ReadTextFileRequest, ReleaseTerminalRequest, RequestPermissionRequest,
    SessionConfigOptionsCapabilities, SessionId, SessionNotification, TerminalOutputRequest,
    WaitForTerminalExitRequest, WriteTextFileRequest,
};
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectTo, ConnectionTo, Error, LineDirection,
};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::bus::EventTx;
use crate::event::{BridgeEvent, UiCmd};
use crate::host::fs::FileSystem;
use crate::host::terminal::Terminals;
use crate::launch::{AgentLaunch, SessionOptions};

#[derive(Default)]
struct Shared {
    session: Mutex<Option<SessionId>>,
    pending: pending::Registry,
}

impl Shared {
    fn active(&self, id: &SessionId) -> bool {
        self.session.lock().as_ref() == Some(id)
    }
}

/// Launch an ACP process, restarting it on request. Dropping the connection
/// terminates the SDK-owned process group (including wrapper descendants).
pub async fn run(
    launch: AgentLaunch,
    options: SessionOptions,
    events: EventTx,
    cmds: UnboundedReceiver<UiCmd>,
) -> anyhow::Result<()> {
    run_recorded(launch, options, events, cmds, None).await
}

/// Like [`run`], optionally appending a redacted wire transcript for fixture capture.
pub async fn run_recorded(
    launch: AgentLaunch,
    options: SessionOptions,
    events: EventTx,
    mut cmds: UnboundedReceiver<UiCmd>,
    record: Option<PathBuf>,
) -> anyhow::Result<()> {
    let record = record.or_else(|| std::env::var_os("ACP_TSP_RECORD").map(PathBuf::from));
    let recorder = record.map(Recorder::open).transpose()?;
    loop {
        let config = AcpAgentConfig::new(&launch.command)
            .args(launch.args.clone())
            .envs(launch.env.clone());
        let mut agent = AcpAgent::new(config);
        if let Some(recorder) = &recorder {
            let recorder = recorder.clone();
            agent = agent.with_debug(move |line, direction| recorder.record(line, direction));
        }
        let restart = drive(agent, options.clone(), events.clone(), &mut cmds).await?;
        if !restart {
            return Ok(());
        }
    }
}

#[derive(Clone)]
struct Recorder {
    writer: Arc<Mutex<std::fs::File>>,
    started: Instant,
}

impl Recorder {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        use std::fs::OpenOptions;
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let writer = options.open(path)?;
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            started: Instant::now(),
        })
    }

    fn record(&self, line: &str, direction: LineDirection) {
        let dir = match direction {
            LineDirection::Stdin => "out",
            LineDirection::Stdout => "in",
            LineDirection::Stderr => "stderr",
        };
        // Stderr is not protocol JSON: it can carry arbitrary credentials.
        let sanitized = if dir == "stderr" {
            "[redacted]".to_owned()
        } else {
            let Ok(mut json) = serde_json::from_str::<serde_json::Value>(line) else {
                self.write_line(dir, "[redacted]");
                return;
            };
            redact(&mut json);
            json.to_string()
        };
        self.write_line(dir, &sanitized);
    }

    fn write_line(&self, dir: &str, sanitized: &str) {
        let entry = serde_json::json!({
            "t": self.started.elapsed().as_millis(),
            "dir": dir,
            "line": sanitized,
        });
        let _ = writeln!(self.writer.lock(), "{entry}");
    }
}

fn redact(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            if fields.get("method").and_then(serde_json::Value::as_str)
                == Some("_auth/status_update")
            {
                fields.insert("params".into(), serde_json::json!({"redacted": true}));
            }
            for (key, child) in fields {
                if key.eq_ignore_ascii_case("email") {
                    *child = serde_json::Value::String("[redacted]".into());
                } else {
                    redact(child);
                }
            }
        }
        serde_json::Value::Array(values) => values.iter_mut().for_each(redact),
        _ => {}
    }
}

/// Run against any SDK transport, e.g. an in-process `Channel`.
pub async fn run_with(
    transport: impl ConnectTo<Client> + 'static,
    options: SessionOptions,
    events: EventTx,
    mut cmds: UnboundedReceiver<UiCmd>,
) -> anyhow::Result<()> {
    let _ = drive(transport, options, events, &mut cmds).await?;
    Ok(())
}

pub fn capabilities() -> ClientCapabilities {
    let session = ClientSessionCapabilities::new().config_options(
        SessionConfigOptionsCapabilities::new().boolean(BooleanConfigOptionCapabilities::new()),
    );
    #[cfg(feature = "unstable_session_notices")]
    let session = session.notices(NoticeCapabilities::new());
    #[cfg(feature = "unstable_session_compaction")]
    let session = session.compaction(CompactionCapabilities::new());
    let client = ClientCapabilities::new()
        .meta(
            serde_json::json!({"terminal_output": true, "terminal-auth": true})
                .as_object()
                .cloned(),
        )
        .fs(FileSystemCapabilities::new()
            .read_text_file(true)
            .write_text_file(true))
        .terminal(true)
        .auth(AuthCapabilities::new().terminal(true))
        .elicitation(
            ElicitationCapabilities::new()
                .form(ElicitationFormCapabilities::new())
                .url(ElicitationUrlCapabilities::new()),
        )
        .session(session);
    #[cfg(feature = "unstable_plan_operations")]
    let client = client.plan(PlanCapabilities::new());
    #[cfg(feature = "unstable_subagents")]
    let client = client.subagents(SubagentCapabilities::new());
    client
}

async fn drive(
    transport: impl ConnectTo<Client> + 'static,
    options: SessionOptions,
    events: EventTx,
    cmds: &mut UnboundedReceiver<UiCmd>,
) -> anyhow::Result<bool> {
    let mut options = options;
    options.cwd = std::fs::canonicalize(&options.cwd)?;
    options.additional_dirs = options
        .additional_dirs
        .into_iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<_>>()?;
    let fs = Arc::new(Mutex::new(FileSystem::new(vec![options.cwd.clone()])?));
    let terminals = Terminals::new(
        options.cwd.clone(),
        vec![options.cwd.clone()],
        events.clone(),
    );
    let shared = Arc::new(Shared::default());
    let result = Client
        .builder()
        .on_receive_notification(
            {
                let shared = shared.clone();
                let events = events.clone();
                async move |update: SessionNotification, _cx| {
                    handlers::update(update, &shared, &events).await
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_notification(
            {
                let events = events.clone();
                async move |notification: CompleteElicitationNotification, _cx| {
                    events.send(BridgeEvent::ElicitationComplete(
                        notification.elicitation_id,
                    ));
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_notification(
            {
                let events = events.clone();
                async move |notification: AgentNotification, _cx| {
                    if let AgentNotification::ExtNotification(extension) = notification
                        && let Ok(params) = serde_json::from_str(extension.params.get())
                    {
                        // SDK 3.1 strips the initial `_` while parsing extension
                        // notification methods; restore the wire method for the UI.
                        events.send(BridgeEvent::Extension {
                            method: format!("_{}", extension.method),
                            params,
                        });
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let events = events.clone();
                async move |request: RequestPermissionRequest,
                            responder,
                            cx: ConnectionTo<Agent>| {
                    handlers::permission(request, responder, cx, shared.clone(), events.clone())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let events = events.clone();
                async move |request: CreateElicitationRequest,
                            responder,
                            cx: ConnectionTo<Agent>| {
                    handlers::elicitation(request, responder, cx, shared.clone(), events.clone())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let fs = fs.clone();
                async move |request: ReadTextFileRequest, responder, cx: ConnectionTo<Agent>| {
                    handlers::read(request, responder, cx, &shared, fs.lock().clone())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let fs = fs.clone();
                async move |request: WriteTextFileRequest, responder, cx: ConnectionTo<Agent>| {
                    handlers::write(request, responder, cx, &shared, fs.lock().clone())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let terminals = terminals.clone();
                async move |request: CreateTerminalRequest, responder, cx: ConnectionTo<Agent>| {
                    if !shared.active(&request.session_id) {
                        return responder.respond_with_error(Error::invalid_params());
                    }
                    let cancellation = responder.cancellation();
                    let terminals = terminals.clone();
                    cx.spawn(async move {
                        responder.respond_with_result(
                            cancellation
                                .run_until_cancelled(async move { terminals.create(request) })
                                .await,
                        )
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let terminals = terminals.clone();
                async move |request: TerminalOutputRequest, responder, _cx| {
                    if !shared.active(&request.session_id) {
                        return responder.respond_with_error(Error::invalid_params());
                    }
                    responder.respond_with_result(terminals.output(&request.terminal_id))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let terminals = terminals.clone();
                async move |request: WaitForTerminalExitRequest,
                            responder,
                            cx: ConnectionTo<Agent>| {
                    if !shared.active(&request.session_id) {
                        return responder.respond_with_error(Error::invalid_params());
                    }
                    let cancellation = responder.cancellation();
                    let terminals = terminals.clone();
                    cx.spawn(async move {
                        responder.respond_with_result(
                            cancellation
                                .run_until_cancelled(terminals.wait(&request.terminal_id))
                                .await,
                        )
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let terminals = terminals.clone();
                async move |request: KillTerminalRequest, responder, _cx| {
                    if !shared.active(&request.session_id) {
                        return responder.respond_with_error(Error::invalid_params());
                    }
                    responder.respond_with_result(terminals.kill(&request.terminal_id))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let shared = shared.clone();
                let terminals = terminals.clone();
                async move |request: ReleaseTerminalRequest, responder, _cx| {
                    if !shared.active(&request.session_id) {
                        return responder.respond_with_error(Error::invalid_params());
                    }
                    responder.respond_with_result(terminals.release(&request.terminal_id))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, |cx: ConnectionTo<Agent>| async {
            events.send(BridgeEvent::Status("Initializing agent…".into()));
            let request = cx.send_request(
                InitializeRequest::new(ProtocolVersion::V1)
                    .client_info(Implementation::new("acp-tsp", env!("CARGO_PKG_VERSION")))
                    .client_capabilities(capabilities()),
            );
            let init = match await_startup(request.block_task(), cmds, &events, &shared).await {
                Startup::Response(result) => result?,
                Startup::Restart => return Ok(Some(true)),
                Startup::Shutdown => return Ok(Some(false)),
            };
            if init.protocol_version != ProtocolVersion::V1 {
                return Err(Error::invalid_request().data("unsupported agent protocol version"));
            }
            let methods = init.auth_methods.clone();
            let caps = init.agent_capabilities.clone();
            let steering = init
                .meta
                .as_ref()
                .and_then(|meta| meta.get("steering"))
                .and_then(|steering| steering.get("supported"))
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            if caps.session_capabilities.additional_directories.is_some() {
                let mut roots = vec![options.cwd.clone()];
                roots.extend(options.additional_dirs.clone());
                *fs.lock() = FileSystem::new(roots.clone()).map_err(Error::into_internal_error)?;
                terminals.set_roots(roots);
            }
            events.send(BridgeEvent::Initialized(Box::new(init)));
            session::run(
                cx,
                options,
                (caps, methods, steering),
                shared.clone(),
                terminals.clone(),
                events.clone(),
                cmds,
            )
            .await
        })
        .await;
    shared.pending.reset(&events);
    terminals.kill_all();
    match result {
        Ok(Some(restart)) => Ok(restart),
        other => {
            let reason = match other {
                Ok(None) => "agent connection closed".to_string(),
                Err(err) => err.to_string(),
                Ok(Some(_)) => unreachable!(),
            };
            events.send(BridgeEvent::AgentExited {
                reason: reason.clone(),
            });
            tracing::debug!("agent connection ended");
            loop {
                match cmds.recv().await {
                    Some(UiCmd::Restart) => return Ok(true),
                    Some(UiCmd::Shutdown) | None => return Ok(false),
                    _ => {}
                }
            }
        }
    }
}

enum Startup<T> {
    Response(Result<T, Error>),
    Restart,
    Shutdown,
}

async fn await_startup<T>(
    request: impl Future<Output = Result<T, Error>>,
    cmds: &mut UnboundedReceiver<UiCmd>,
    events: &EventTx,
    shared: &Shared,
) -> Startup<T> {
    tokio::pin!(request);
    loop {
        tokio::select! {
            result = &mut request => return Startup::Response(result),
            cmd = cmds.recv() => match cmd {
                Some(UiCmd::Restart) => return Startup::Restart,
                Some(UiCmd::Shutdown) | None => return Startup::Shutdown,
                Some(UiCmd::Prompt(_)) => { events.send(BridgeEvent::Error("Session is not ready".into())); }
                Some(UiCmd::Permission { id, outcome }) => {
                    shared.pending.answer_permission(id, Ok(outcome));
                }
                Some(UiCmd::Elicitation { id, response }) => {
                    shared.pending.answer_elicitation(id, Ok(response));
                }
                Some(UiCmd::Cancel) => {
                    shared.pending.cancel_turn(events);
                    shared.pending.turn_ended();
                }
                _ => {},
            }
        }
    }
}

#[cfg(test)]
mod recorder_tests {
    use super::*;

    #[test]
    fn status_params_and_nested_emails_never_reach_recordings() {
        let mut update = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "_auth/status_update",
            "params": {"email":"private@example.invalid", "nested":{"email":"private@example.invalid"}}
        });
        redact(&mut update);
        assert_eq!(update["params"], serde_json::json!({"redacted": true}));
        let mut other =
            serde_json::json!({"result": {"account": {"email": "private@example.invalid"}}});
        redact(&mut other);
        assert_eq!(other["result"]["account"]["email"], "[redacted]");
    }
}
