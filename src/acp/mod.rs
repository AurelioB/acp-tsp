//! ACP client connection and turn driver.
mod handlers;
#[cfg(feature = "unstable_mcp_over_acp")]
mod mcp;
mod pending;
mod session;

use parking_lot::Mutex;
use std::sync::Arc;

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
    AuthCapabilities, BooleanConfigOptionCapabilities, ClientCapabilities, ClientNotification,
    ClientSessionCapabilities, CompleteElicitationNotification, CreateElicitationRequest,
    CreateTerminalRequest, ElicitationCapabilities, ElicitationFormCapabilities,
    ElicitationUrlCapabilities, FileSystemCapabilities, Implementation, InitializeRequest,
    KillTerminalRequest, ReadTextFileRequest, ReleaseTerminalRequest, RequestPermissionRequest,
    SessionConfigOptionsCapabilities, SessionId, SessionNotification, TerminalOutputRequest,
    WaitForTerminalExitRequest, WriteTextFileRequest,
};
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectTo, ConnectionTo, Error,
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
    mut cmds: UnboundedReceiver<UiCmd>,
) -> anyhow::Result<()> {
    loop {
        let config = AcpAgentConfig::new(&launch.command)
            .args(launch.args.clone())
            .envs(launch.env.clone());
        let restart = drive(
            AcpAgent::new(config),
            options.clone(),
            events.clone(),
            &mut cmds,
        )
        .await?;
        if !restart {
            return Ok(());
        }
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
            async |notification: ClientNotification, _cx| {
                if let ClientNotification::ExtNotification(extension) = notification {
                    tracing::debug!(method = %extension.method, "ignoring extension notification");
                }
                Ok(())
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
            let init = match await_startup(request.block_task(), cmds, &events).await {
                Startup::Response(result) => result?,
                Startup::Restart => return Ok(Some(true)),
                Startup::Shutdown => return Ok(Some(false)),
            };
            if init.protocol_version != ProtocolVersion::V1 {
                return Err(Error::invalid_request().data("unsupported agent protocol version"));
            }
            let methods = init.auth_methods.clone();
            let caps = init.agent_capabilities.clone();
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
                (caps, methods),
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
) -> Startup<T> {
    tokio::pin!(request);
    loop {
        tokio::select! {
            result = &mut request => return Startup::Response(result),
            cmd = cmds.recv() => match cmd {
                Some(UiCmd::Restart) => return Startup::Restart,
                Some(UiCmd::Shutdown) | None => return Startup::Shutdown,
                Some(UiCmd::Prompt(_)) => { events.send(BridgeEvent::Error("Session is not ready".into())); }
                _ => {},
            }
        }
    }
}
