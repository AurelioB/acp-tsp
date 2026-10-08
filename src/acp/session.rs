//! Outgoing ACP requests and the active-session state machine.
use std::collections::VecDeque;
use std::sync::Arc;

#[cfg(feature = "unstable_session_fork")]
use agent_client_protocol::schema::v1::ForkSessionRequest;
#[cfg(feature = "unstable_llm_providers")]
use agent_client_protocol::schema::v1::ListProvidersRequest;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, AuthMethod, AuthenticateRequest, CancelNotification, CloseSessionRequest,
    ContentBlock, DeleteSessionRequest, ListSessionsRequest, LoadSessionRequest, LogoutRequest,
    McpServer, NewSessionRequest, PromptRequest, PromptResponse, ResumeSessionRequest,
    SessionConfigOption, SessionConfigOptionValue, SessionId, SessionModeState,
    SetSessionConfigOptionRequest, SetSessionModeRequest,
};
use agent_client_protocol::{Agent, ConnectionTo, Error};
#[cfg(feature = "unstable_mcp_over_acp")]
use agent_client_protocol::{NullRun, mcp_server::McpServer as InProcessMcpServer};
#[cfg(feature = "unstable_mcp_over_acp")]
use agent_client_protocol::{SessionMessage, util::MatchDispatch};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use super::Shared;
use crate::bus::EventTx;
use crate::event::{BridgeEvent, SessionStartKind, UiCmd};
use crate::host::terminal::Terminals;
use crate::launch::{SessionOptions, StartSession};
struct ScopedMcp(tokio::task::JoinHandle<()>);
impl Drop for ScopedMcp {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) async fn run(
    cx: ConnectionTo<Agent>,
    options: SessionOptions,
    agent: (AgentCapabilities, Vec<AuthMethod>),
    shared: Arc<Shared>,
    terminals: Terminals,
    events: EventTx,
    cmds: &mut UnboundedReceiver<UiCmd>,
) -> Result<Option<bool>, Error> {
    let (caps, methods) = agent;
    let dirs = if caps.session_capabilities.additional_directories.is_some() {
        options.additional_dirs.clone()
    } else {
        if !options.additional_dirs.is_empty() {
            events.send(BridgeEvent::Error(
                "Agent does not support additional directories".into(),
            ));
        }
        Vec::new()
    };
    let servers = options
        .mcp_servers
        .iter()
        .filter(|server| match server {
            McpServer::Stdio(_) => true,
            McpServer::Http(_) => caps.mcp_capabilities.http,
            McpServer::Sse(_) => caps.mcp_capabilities.sse,
            #[cfg(feature = "unstable_mcp_over_acp")]
            McpServer::Acp(_) => caps.mcp_capabilities.acp,
            _ => false,
        })
        .cloned()
        .collect::<Vec<_>>();
    if servers.len() != options.mcp_servers.len() {
        events.send(BridgeEvent::Error(
            "Unsupported MCP transports were dropped".into(),
        ));
    }
    let mut selected = options.start.clone();
    if selected == StartSession::Latest {
        if caps.session_capabilities.list.is_some() {
            let mut cursor = None;
            let mut newest = None;
            loop {
                let result = cx
                    .send_request(
                        ListSessionsRequest::new()
                            .cwd(options.cwd.clone())
                            .cursor(cursor),
                    )
                    .block_task()
                    .await?;
                for info in result.sessions.into_iter().filter(|s| s.cwd == options.cwd) {
                    if newest.as_ref().is_none_or(
                        |old: &agent_client_protocol::schema::v1::SessionInfo| {
                            info.updated_at > old.updated_at
                        },
                    ) {
                        newest = Some(info);
                    }
                }
                cursor = result.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            selected = newest
                .map(|s| StartSession::Load(s.session_id))
                .unwrap_or(StartSession::New);
        } else {
            events.send(BridgeEvent::Error(
                "Agent does not support session/list".into(),
            ));
            selected = StartSession::New;
        }
    }
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
    let mut queued: VecDeque<Vec<ContentBlock>> = VecDeque::new();
    let mut running = false;
    let mut active: Option<SessionId> = None;
    let mut modes: Option<SessionModeState> = None;
    let mut configs: Option<Vec<SessionConfigOption>> = None;
    // Dropping the task drops SentRequest and sends $/cancel_request for obsolete pages.
    let mut list_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut mcp_task: Option<ScopedMcp> = None;
    loop {
        if active.is_none() && selected != StartSession::Latest {
            events.send(BridgeEvent::Status("Creating session…".into()));
            let kind = match &selected {
                StartSession::New => SessionStartKind::New,
                StartSession::Load(_) if caps.load_session => SessionStartKind::Load,
                StartSession::Load(_) => SessionStartKind::Resume,
                StartSession::Resume(_) => SessionStartKind::Resume,
                StartSession::Latest => unreachable!(),
            };
            if kind == SessionStartKind::Resume && caps.session_capabilities.resume.is_none() {
                events.send(BridgeEvent::Error(
                    "Agent does not support session/resume".into(),
                ));
                selected = StartSession::Latest;
                continue;
            }
            if let StartSession::Load(id) = &selected
                && kind == SessionStartKind::Load
            {
                events.send(BridgeEvent::SessionStarting(kind));
                *shared.session.lock() = Some(id.clone());
            }
            let setup = match &selected {
                StartSession::New => {
                    let request = NewSessionRequest::new(&options.cwd)
                        .additional_directories(dirs.clone())
                        .mcp_servers(servers.clone());
                    #[cfg(feature = "unstable_mcp_over_acp")]
                    let response = if caps.mcp_capabilities.acp {
                        let roots = std::iter::once(options.cwd.clone())
                            .chain(dirs.iter().cloned())
                            .collect();
                        match cx
                            .build_session_from(request)
                            .with_mcp_server(InProcessMcpServer::new_service(
                                "tern",
                                super::mcp::TernTools::new(roots),
                                NullRun,
                            ))?
                            .block_task()
                            .start_session()
                            .await
                        {
                            Ok(mut session) => {
                                let response = session.response();
                                let shared = shared.clone();
                                let events = events.clone();
                                mcp_task = Some(ScopedMcp(tokio::spawn(async move {
                                    while let Ok(message) = session.read_update().await {
                                        if let SessionMessage::SessionMessage(dispatch) = message {
                                            let events = events.clone();
                                            let shared = shared.clone();
                                            let _ = MatchDispatch::new(dispatch)
                                                .if_notification(async move |notification: agent_client_protocol::schema::v1::SessionNotification| {
                                                    super::handlers::update(notification, &shared, &events).await
                                                }).await.otherwise_ignore();
                                        }
                                    }
                                })));
                                Ok(response)
                            }
                            Err(err) => Err(err),
                        }
                    } else {
                        cx.send_request(request).block_task().await
                    };
                    #[cfg(not(feature = "unstable_mcp_over_acp"))]
                    let response = cx.send_request(request).block_task().await;
                    response.map(|s| (s.session_id, s.modes, s.config_options))
                }
                StartSession::Load(id) if kind == SessionStartKind::Load => cx
                    .send_request(
                        LoadSessionRequest::new(id.clone(), &options.cwd)
                            .additional_directories(dirs.clone())
                            .mcp_servers(servers.clone()),
                    )
                    .block_task()
                    .await
                    .map(|s| (id.clone(), s.modes, s.config_options)),
                StartSession::Load(id) | StartSession::Resume(id) => cx
                    .send_request(
                        ResumeSessionRequest::new(id.clone(), &options.cwd)
                            .additional_directories(dirs.clone())
                            .mcp_servers(servers.clone()),
                    )
                    .block_task()
                    .await
                    .map(|s| (id.clone(), s.modes, s.config_options)),
                StartSession::Latest => unreachable!(),
            };
            match setup {
                Ok((id, new_modes, new_configs)) => {
                    active = Some(id.clone());
                    *shared.session.lock() = Some(id.clone());
                    modes = new_modes.clone();
                    configs = new_configs.clone();
                    events.send(BridgeEvent::SessionStarted {
                        session_id: id,
                        kind,
                        cwd: options.cwd.clone(),
                        additional_dirs: dirs.clone(),
                        modes: new_modes,
                        config_options: new_configs,
                    });
                    selected = StartSession::Latest;
                }
                Err(err)
                    if err.code == Error::auth_required().code
                        || (!methods.is_empty() && is_auth_error(&err)) =>
                {
                    *shared.session.lock() = None;
                    events.send(BridgeEvent::AuthRequired {
                        methods: methods.clone(),
                        message: Some(err.message),
                    });
                    loop {
                        tokio::select! {
                            command = cmds.recv() => match command {
                                Some(UiCmd::Authenticate(id)) => {
                                    if !methods.iter().any(|m| matches!(m, AuthMethod::Agent(agent) if agent.id == id)) {
                                        events.send(BridgeEvent::Error("Unsupported authentication method".into()));
                                        continue;
                                    }
                                    match cx.send_request(AuthenticateRequest::new(id)).block_task().await {
                                        Ok(_) => break,
                                        Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                                    }
                                }
                                Some(UiCmd::RetrySession) => break,
                                Some(UiCmd::Restart) => return Ok(Some(true)),
                                Some(UiCmd::Shutdown) | None => return Ok(Some(false)),
                                Some(UiCmd::Prompt(_)) => { events.send(BridgeEvent::Error("Session is not ready".into())); }
                                _ => {},
                            },
                            _ = cx.incoming_closed() => return Ok(None),
                        }
                    }
                }
                Err(err) => {
                    *shared.session.lock() = None;
                    events.send(BridgeEvent::Error(err.to_string()));
                    selected = StartSession::Latest;
                }
            }
            continue;
        }
        tokio::select! {
            cmd = cmds.recv() => {
                if matches!(&cmd, Some(UiCmd::NewSession | UiCmd::LoadSession(_) |
                    UiCmd::ResumeSession(_) | UiCmd::CloseSession | UiCmd::DeleteSession(_) |
                    UiCmd::ForkSession | UiCmd::Restart | UiCmd::Shutdown))
                    && let Some(task) = list_task.take() {
                    task.abort();
                }
                match cmd {
                Some(UiCmd::Prompt(prompt)) => {
                    if let Some(id) = &active {
                        queued.push_back(prompt);
                        if !running {
                            start_prompt(&cx, id, queued.pop_front().expect("queued prompt"), &turn_tx)?;
                            running = true;
                        }
                    } else { events.send(BridgeEvent::Error("Session is not ready".into())); }
                }
                Some(UiCmd::Cancel) => {
                    queued.clear();
                    shared.pending.cancel_turn(&events);
                    if running {
                        if let Some(id) = &active { cx.send_notification(CancelNotification::new(id.clone()))?; }
                    } else { shared.pending.turn_ended(); }
                }
                Some(UiCmd::Permission { id, outcome }) => { shared.pending.answer_permission(id, Ok(outcome)); }
                Some(UiCmd::Elicitation { id, response }) => { shared.pending.answer_elicitation(id, Ok(response)); }
                Some(UiCmd::KillTerminal(id)) => {
                    if let Err(err) = terminals.kill(&id) { events.send(BridgeEvent::Error(err.to_string())); }
                }
                Some(UiCmd::NewSession) | Some(UiCmd::LoadSession(_)) | Some(UiCmd::ResumeSession(_)) | Some(UiCmd::ForkSession) | Some(UiCmd::CloseSession) | Some(UiCmd::DeleteSession(_)) if running => {
                    events.send(BridgeEvent::Error("Wait for the current turn before changing sessions".into()));
                }
                Some(UiCmd::NewSession) => {
                    events.send(BridgeEvent::SessionStarting(SessionStartKind::New));
                    switch(&shared, &terminals, &events, &mut mcp_task);
                    active = None; selected = StartSession::New;
                }
                Some(UiCmd::LoadSession(id)) => {
                    if !caps.load_session && caps.session_capabilities.resume.is_none() {
                        events.send(BridgeEvent::Error("Agent does not support session/load or resume".into()));
                    } else {
                        switch(&shared, &terminals, &events, &mut mcp_task);
                        active = None; selected = StartSession::Load(id);
                    }
                }
                Some(UiCmd::ResumeSession(id)) => {
                    if caps.session_capabilities.resume.is_none() {
                        events.send(BridgeEvent::Error("Agent does not support session/resume".into()));
                    } else {
                        events.send(BridgeEvent::SessionStarting(SessionStartKind::Resume));
                        switch(&shared, &terminals, &events, &mut mcp_task);
                        active = None; selected = StartSession::Resume(id);
                    }
                }
                Some(UiCmd::ListSessions { cursor }) => {
                    if caps.session_capabilities.list.is_none() {
                        events.send(BridgeEvent::Error("Agent does not support session/list".into()));
                    } else {
                        if let Some(task) = list_task.take() { task.abort(); }
                        let cx = cx.clone(); let events = events.clone(); let cwd = options.cwd.clone();
                        list_task = Some(tokio::spawn(async move {
                            let result = cx.send_request(ListSessionsRequest::new().cwd(cwd.clone()).cursor(cursor.clone())).block_task().await;
                            match result {
                                Ok(page) => { events.send(BridgeEvent::SessionList {
                                    sessions: page.sessions.into_iter().filter(|s| s.cwd == cwd).collect(),
                                    next_cursor: page.next_cursor, first_page: cursor.is_none(),
                                }); }
                                Err(err) if err.code == Error::request_cancelled().code => {},
                                Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                            }
                        }));
                    }
                }
                Some(UiCmd::CloseSession) => {
                    if caps.session_capabilities.close.is_none() {
                        events.send(BridgeEvent::Error("Agent does not support session/close".into()));
                    } else if let Some(id) = &active {
                        match cx.send_request(CloseSessionRequest::new(id.clone())).block_task().await {
                            Ok(_) => { switch(&shared, &terminals, &events, &mut mcp_task); active = None; events.send(BridgeEvent::SessionClosed); },
                            Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                        }
                    } else { events.send(BridgeEvent::Error("Session is not ready".into())); }
                }
                Some(UiCmd::DeleteSession(id)) => {
                    if caps.session_capabilities.delete.is_none() {
                        events.send(BridgeEvent::Error("Agent does not support session/delete".into()));
                    } else {
                        if active.as_ref() == Some(&id) {
                            switch(&shared, &terminals, &events, &mut mcp_task); active = None;
                            events.send(BridgeEvent::SessionClosed);
                        }
                        match cx.send_request(DeleteSessionRequest::new(id.clone())).block_task().await {
                            Ok(_) => { events.send(BridgeEvent::SessionDeleted(id)); }
                            Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                        }
                    }
                }
                Some(UiCmd::ForkSession) => {
                    #[cfg(feature = "unstable_session_fork")]
                    if let Some(id) = &active {
                        if caps.session_capabilities.fork.is_some() {
                            events.send(BridgeEvent::SessionStarting(SessionStartKind::Fork));
                            match cx.send_request(ForkSessionRequest::new(id.clone(), &options.cwd)
                                .additional_directories(dirs.clone()).mcp_servers(servers.clone())).block_task().await {
                                Ok(s) => {
                                    switch(&shared, &terminals, &events, &mut mcp_task);
                                    *shared.session.lock() = Some(s.session_id.clone());
                                    active = Some(s.session_id.clone()); modes = s.modes.clone(); configs = s.config_options.clone();
                                    events.send(BridgeEvent::SessionStarted { session_id: s.session_id, kind: SessionStartKind::Fork,
                                        cwd: options.cwd.clone(), additional_dirs: dirs.clone(), modes: s.modes, config_options: s.config_options });
                                }
                                Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                            }
                        } else { events.send(BridgeEvent::Error("Agent does not support session/fork".into())); }
                    } else { events.send(BridgeEvent::Error("Session is not ready".into())); }
                    #[cfg(not(feature = "unstable_session_fork"))]
                    events.send(BridgeEvent::Error("session/fork is not enabled".into()));
                }
                Some(UiCmd::SetMode(mode)) => {
                    if let Some(id) = &active {
                        if modes.is_none() { events.send(BridgeEvent::Error("Agent did not advertise modes".into())); }
                        else { match cx.send_request(SetSessionModeRequest::new(id.clone(), mode.clone())).block_task().await {
                            Ok(_) => { events.send(BridgeEvent::ModeChanged(mode)); }
                            Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                        } }
                    } else { events.send(BridgeEvent::Error("Session is not ready".into())); }
                }
                Some(UiCmd::SetConfigOption { config_id, value }) => {
                    if let Some(id) = &active {
                        let allowed = configs.as_ref().is_some_and(|options| options.iter().any(|o| o.id == config_id &&
                            (!matches!(value, SessionConfigOptionValue::Boolean { .. }) ||
                                matches!(o.kind, agent_client_protocol::schema::v1::SessionConfigKind::Boolean(_)))));
                        if !allowed { events.send(BridgeEvent::Error("Agent did not advertise this config option".into())); }
                        else { match cx.send_request(SetSessionConfigOptionRequest::new(id.clone(), config_id, value)).block_task().await {
                            Ok(result) => { configs = Some(result.config_options.clone()); events.send(BridgeEvent::ConfigOptions(result.config_options)); }
                            Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                        } }
                    } else { events.send(BridgeEvent::Error("Session is not ready".into())); }
                }
                Some(UiCmd::Logout) => {
                    if caps.auth.logout.is_none() { events.send(BridgeEvent::Error("Agent does not support logout".into())); }
                    else { match cx.send_request(LogoutRequest::new()).block_task().await {
                        Ok(_) => { switch(&shared, &terminals, &events, &mut mcp_task); active = None; selected = StartSession::New; events.send(BridgeEvent::LoggedOut); },
                        Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                    } }
                }
                #[cfg(feature = "unstable_llm_providers")]
                Some(UiCmd::ListProviders) => {
                    if caps.providers.is_none() { events.send(BridgeEvent::Error("Agent does not support providers".into())); }
                    else { match cx.send_request(ListProvidersRequest::new()).block_task().await {
                        Ok(result) => { events.send(BridgeEvent::Providers(Box::new(result))); }
                        Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                    } }
                }
                #[cfg(feature = "unstable_llm_providers")]
                Some(UiCmd::SetProvider(request)) => {
                    if caps.providers.is_none() { events.send(BridgeEvent::Error("Agent does not support providers".into())); }
                    else { match cx.send_request(*request).block_task().await {
                        Ok(_) => refresh_providers(&cx, &events).await,
                        Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                    } }
                }
                #[cfg(feature = "unstable_llm_providers")]
                Some(UiCmd::DisableProvider(request)) => {
                    if caps.providers.is_none() { events.send(BridgeEvent::Error("Agent does not support providers".into())); }
                    else { match cx.send_request(*request).block_task().await {
                        Ok(_) => refresh_providers(&cx, &events).await,
                        Err(err) => { events.send(BridgeEvent::Error(err.to_string())); }
                    } }
                }
                Some(UiCmd::Authenticate(_)) => { events.send(BridgeEvent::Error("Session is already authenticated".into())); }
                Some(UiCmd::RetrySession) => { events.send(BridgeEvent::Error("Authentication is not requested".into())); }
                Some(UiCmd::Restart) => return Ok(Some(true)),
                Some(UiCmd::Shutdown) | None => return Ok(Some(false)),
                }
            }
            result = turn_rx.recv(), if running => {
                if let Some(result) = result {
                    events.send(BridgeEvent::TurnEnded(result.map(Box::new).map_err(|err| err.to_string())));
                    running = false; shared.pending.turn_ended();
                    if let Some(next) = queued.pop_front()
                        && let Some(id) = &active
                    {
                        start_prompt(&cx, id, next, &turn_tx)?;
                        running = true;
                    }
                }
            }
            _ = cx.incoming_closed() => return Ok(None),
        }
    }
}

#[cfg(feature = "unstable_llm_providers")]
async fn refresh_providers(cx: &ConnectionTo<Agent>, events: &EventTx) {
    match cx
        .send_request(ListProvidersRequest::new())
        .block_task()
        .await
    {
        Ok(result) => {
            events.send(BridgeEvent::Providers(Box::new(result)));
        }
        Err(err) => {
            events.send(BridgeEvent::Error(err.to_string()));
        }
    }
}

fn switch(
    shared: &Shared,
    terminals: &Terminals,
    events: &EventTx,
    mcp_task: &mut Option<ScopedMcp>,
) {
    *shared.session.lock() = None;
    shared.pending.reset(events);
    terminals.kill_all();
    mcp_task.take();
}

fn start_prompt(
    cx: &ConnectionTo<Agent>,
    session: &SessionId,
    prompt: Vec<ContentBlock>,
    result_tx: &mpsc::UnboundedSender<Result<PromptResponse, Error>>,
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
