//! ACP client connection and turn driver.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthenticateRequest, CancelNotification, ClientCapabilities, ClientNotification,
    ContentBlock, FileSystemCapabilities, Implementation, InitializeRequest, NewSessionRequest,
    PromptRequest, ReadTextFileRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionId, SessionNotification, WriteTextFileRequest,
};
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectTo, ConnectionTo, Error, Responder,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::bus::EventTx;
use crate::event::{BridgeEvent, PendingId, UiCmd};
use crate::host::fs::FileSystem;
use crate::launch::AgentLaunch;

static NEXT_PERMISSION_ID: AtomicU64 = AtomicU64::new(1);

type PermissionResponder = Responder<RequestPermissionResponse>;
struct PendingPermission {
    responder: PermissionResponder,
    done: tokio::sync::oneshot::Sender<()>,
}

#[derive(Default)]
struct Shared {
    session: Mutex<Option<SessionId>>,
    permissions: Mutex<BTreeMap<PendingId, PendingPermission>>,
}

impl Shared {
    fn resolve_all(&self, events: &EventTx) {
        let pending = std::mem::take(&mut *self.permissions.lock().expect("permission lock"));
        for (id, pending) in pending {
            let _ = pending.done.send(());
            if let Err(err) = pending.responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            )) {
                tracing::debug!(%err, "permission response after disconnect");
            }
            events.send(BridgeEvent::Resolved(id));
        }
    }

    fn answer(&self, id: PendingId, result: Result<RequestPermissionOutcome, Error>) -> bool {
        let Some(pending) = self
            .permissions
            .lock()
            .expect("permission lock")
            .remove(&id)
        else {
            return false;
        };
        let _ = pending.done.send(());
        let response = result.map(RequestPermissionResponse::new);
        if let Err(err) = pending.responder.respond_with_result(response) {
            tracing::debug!(%err, "permission response after disconnect");
        }
        true
    }
}

/// Launch an ACP process, restarting it on request. Dropping the connection
/// terminates the SDK-owned process group (including wrapper descendants).
pub async fn run(
    launch: AgentLaunch,
    cwd: PathBuf,
    events: EventTx,
    mut cmds: UnboundedReceiver<UiCmd>,
) -> anyhow::Result<()> {
    loop {
        let config = AcpAgentConfig::new(&launch.command)
            .args(launch.args.clone())
            .envs(launch.env.clone());
        let mut agent = AcpAgent::new(config);
        if std::env::var("ACP_TSP_TRACE_WIRE").as_deref() == Ok("1") {
            agent =
                agent.with_debug(|line, direction| tracing::trace!(?direction, line, "ACP wire"));
        }
        let restart = drive(agent, cwd.clone(), events.clone(), &mut cmds).await?;
        if !restart {
            return Ok(());
        }
    }
}

/// Run the same client against any SDK transport, e.g. an in-process `Channel`.
/// A one-shot transport cannot be respawned; `Restart` closes it and returns.
pub async fn run_with(
    transport: impl ConnectTo<Client> + 'static,
    cwd: PathBuf,
    events: EventTx,
    mut cmds: UnboundedReceiver<UiCmd>,
) -> anyhow::Result<()> {
    let _ = drive(transport, cwd, events, &mut cmds).await?;
    Ok(())
}

async fn drive(
    transport: impl ConnectTo<Client> + 'static,
    cwd: PathBuf,
    events: EventTx,
    cmds: &mut UnboundedReceiver<UiCmd>,
) -> anyhow::Result<bool> {
    let cwd = std::fs::canonicalize(cwd)?;
    let fs = FileSystem::new(vec![cwd.clone()])?;
    let shared = Arc::new(Shared::default());
    let updates = shared.clone();
    let permissions = shared.clone();
    let reads = shared.clone();
    let writes = shared.clone();
    let command_rx = &mut *cmds;
    let session_events = events.clone();
    let session_shared = shared.clone();
    let result = Client.builder()
        .on_receive_notification(
            {
                let events = events.clone();
                async move |notification: SessionNotification, _cx| {
                    if updates.session.lock().expect("session lock").as_ref() == Some(&notification.session_id) {
                        events.send(BridgeEvent::Update(Box::new(notification.update)));
                    } else {
                        tracing::debug!(session = %notification.session_id, "ignoring update for inactive session");
                    }
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
                let events = events.clone();
                async move |request: RequestPermissionRequest, responder: PermissionResponder, cx: ConnectionTo<Agent>| {
                    if permissions.session.lock().expect("session lock").as_ref() != Some(&request.session_id) {
                        return responder.respond_with_error(Error::invalid_params());
                    }
                    let id = NEXT_PERMISSION_ID.fetch_add(1, Ordering::Relaxed);
                    let cancellation = responder.cancellation();
                    let (done, closed) = tokio::sync::oneshot::channel();
                    permissions.permissions.lock().expect("permission lock").insert(id, PendingPermission { responder, done });
                    events.send(BridgeEvent::Permission { id, request: Box::new(request) });
                    let pending = permissions.clone();
                    let events = events.clone();
                    cx.spawn(async move {
                        tokio::select! {
                            _ = cancellation.cancelled() => {
                                if pending.answer(id, Err(Error::request_cancelled())) {
                                    events.send(BridgeEvent::Resolved(id));
                                }
                            }
                            _ = closed => {}
                        }
                        Ok(())
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let fs = fs.clone();
                async move |request: ReadTextFileRequest, responder, cx: ConnectionTo<Agent>| {
                    if reads.session.lock().expect("session lock").as_ref() != Some(&request.session_id) {
                        return responder.respond_with_error(Error::invalid_params());
                    }
                    let cancellation = responder.cancellation();
                    let fs = fs.clone();
                    cx.spawn(async move {
                        let result = cancellation.run_until_cancelled(async move {
                            tokio::task::spawn_blocking(move || fs.read(request)).await
                                .map_err(Error::into_internal_error)?
                        }).await;
                        responder.respond_with_result(result)
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                async move |request: WriteTextFileRequest, responder, cx: ConnectionTo<Agent>| {
                    if writes.session.lock().expect("session lock").as_ref() != Some(&request.session_id) {
                        return responder.respond_with_error(Error::invalid_params());
                    }
                    let cancellation = responder.cancellation();
                    let fs = fs.clone();
                    cx.spawn(async move {
                        let result = cancellation.run_until_cancelled(async move {
                            tokio::task::spawn_blocking(move || fs.write(request)).await
                                .map_err(Error::into_internal_error)?
                        }).await;
                        responder.respond_with_result(result)
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, |cx: ConnectionTo<Agent>| async move {
            let cmds = command_rx;
            let events = session_events;
            let shared = session_shared;
            events.send(BridgeEvent::Status("Initializing agent…".into()));
            let initialize = cx.send_request(
                InitializeRequest::new(ProtocolVersion::V1)
                    .client_info(Implementation::new("acp-tsp", env!("CARGO_PKG_VERSION")))
                    .client_capabilities(ClientCapabilities::new().fs(
                        FileSystemCapabilities::new().read_text_file(true).write_text_file(true)
                    ))
            );
            let init = match await_startup(initialize.block_task(), cmds, &events).await {
                Startup::Response(result) => result?,
                Startup::Restart => return Ok(Some(true)),
                Startup::Shutdown => return Ok(Some(false)),
            };
            if init.protocol_version != ProtocolVersion::V1 {
                return Err(Error::invalid_request().data("unsupported agent protocol version"));
            }
            let auth_methods: Vec<AuthMethod> = init.auth_methods.clone().into_iter()
                .filter(|method| matches!(method, AuthMethod::Agent(_))).collect();
            events.send(BridgeEvent::Initialized(Box::new(init)));
            let session_id = loop {
                events.send(BridgeEvent::Status("Creating session…".into()));
                let new_session = cx.send_request(NewSessionRequest::new(&cwd));
                let session = match await_startup(new_session.block_task(), cmds, &events).await {
                    Startup::Response(result) => result,
                    Startup::Restart => return Ok(Some(true)),
                    Startup::Shutdown => return Ok(Some(false)),
                };
                match session {
                    Ok(session) => {
                        *shared.session.lock().expect("session lock") = Some(session.session_id.clone());
                        events.send(BridgeEvent::SessionStarted {
                            session_id: session.session_id.clone(), cwd: cwd.clone(),
                            modes: session.modes, config_options: session.config_options,
                        });
                        break session.session_id;
                    }
                    Err(err) if err.code == Error::auth_required().code || (!auth_methods.is_empty() && is_auth_error(&err)) => {
                        events.send(BridgeEvent::AuthRequired { methods: auth_methods.clone(), message: Some(err.message) });
                        loop {
                            tokio::select! {
                                cmd = cmds.recv() => match cmd {
                                    Some(UiCmd::Authenticate(id)) => {
                                        if !auth_methods.iter().any(|method| matches!(method, AuthMethod::Agent(agent) if agent.id == id)) {
                                            events.send(BridgeEvent::Error("Unsupported authentication method".into()));
                                            continue;
                                        }
                                        events.send(BridgeEvent::Status("Authenticating…".into()));
                                        let authenticate = cx.send_request(AuthenticateRequest::new(id));
                                        match await_startup(authenticate.block_task(), cmds, &events).await {
                                            Startup::Response(Ok(_)) => break,
                                            Startup::Response(Err(err)) => { events.send(BridgeEvent::Error(err.to_string())); }
                                            Startup::Restart => return Ok(Some(true)),
                                            Startup::Shutdown => return Ok(Some(false)),
                                        }
                                    }
                                    Some(UiCmd::Restart) => return Ok(Some(true)),
                                    Some(UiCmd::Shutdown) | None => return Ok(Some(false)),
                                    Some(UiCmd::Prompt(_)) => { events.send(BridgeEvent::Error("Session is not ready".into())); }
                                    Some(UiCmd::Permission { id, .. }) => tracing::debug!(id, "ignoring unknown permission"),
                                    Some(UiCmd::Cancel) => {},
                                },
                                _ = cx.incoming_closed() => return Ok(None),
                            }
                        }
                    }
                    Err(err) => return Err(err),
                }
            };
            let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
            let mut queued: VecDeque<Vec<ContentBlock>> = VecDeque::new();
            let mut running = false;
            loop {
                tokio::select! {
                    cmd = cmds.recv() => match cmd {
                        Some(UiCmd::Prompt(prompt)) => {
                            queued.push_back(prompt);
                            if !running {
                                start_prompt(&cx, &session_id, queued.pop_front().expect("queued prompt"), &turn_tx)?;
                                running = true;
                            }
                        }
                        Some(UiCmd::Cancel) => {
                            queued.clear();
                            shared.resolve_all(&events);
                            if running { cx.send_notification(CancelNotification::new(session_id.clone()))?; }
                        }
                        Some(UiCmd::Permission { id, outcome }) => {
                            if !shared.answer(id, Ok(outcome)) { tracing::debug!(id, "ignoring unknown permission"); }
                        }
                        Some(UiCmd::Authenticate(_)) => { events.send(BridgeEvent::Error("Session is already authenticated".into())); }
                        Some(UiCmd::Restart) => return Ok(Some(true)),
                        Some(UiCmd::Shutdown) | None => return Ok(Some(false)),
                    },
                    result = turn_rx.recv(), if running => {
                        if let Some(result) = result {
                            events.send(BridgeEvent::TurnEnded(result.map(|r| r.stop_reason).map_err(|e| e.to_string())));
                            running = false;
                            if let Some(next) = queued.pop_front() {
                                start_prompt(&cx, &session_id, next, &turn_tx)?;
                                running = true;
                            }
                        }
                    }
                    _ = cx.incoming_closed() => return Ok(None),
                }
            }
        }).await;
    shared.resolve_all(&events);
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
            tracing::debug!(%reason, "agent connection ended");
            // Keep the UI alive to offer a restart, but never reuse the old transport.
            loop {
                match cmds.recv().await {
                    Some(UiCmd::Restart) => return Ok(true),
                    Some(UiCmd::Shutdown) | None => return Ok(false),
                    Some(UiCmd::Permission { id, .. }) => {
                        tracing::debug!(id, "ignoring stale permission")
                    }
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
                Some(UiCmd::Permission { id, .. }) => tracing::debug!(id, "ignoring unknown permission"),
                Some(UiCmd::Authenticate(_)) => { events.send(BridgeEvent::Error("Authentication is not requested".into())); }
                Some(UiCmd::Cancel) => {},
            }
        }
    }
}

fn start_prompt(
    cx: &ConnectionTo<Agent>,
    session: &SessionId,
    prompt: Vec<ContentBlock>,
    result_tx: &mpsc::UnboundedSender<
        Result<agent_client_protocol::schema::v1::PromptResponse, Error>,
    >,
) -> Result<(), Error> {
    let request = cx.send_request(PromptRequest::new(session.clone(), prompt));
    let result_tx = result_tx.clone();
    cx.spawn(async move {
        let _ = result_tx.send(request.block_task().await);
        Ok(())
    })
}

fn is_auth_error(error: &Error) -> bool {
    let lower = error.message.to_ascii_lowercase();
    [
        "auth",
        "login",
        "log in",
        "sign in",
        "signed in",
        "credentials",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}
