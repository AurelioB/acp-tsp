use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use acp_tsp::{
    acp,
    bus::{EventRx, event_channel, wake_pair},
    event::{BridgeEvent, UiCmd},
    launch::SessionOptions,
};
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AgentAuthCapabilities, AgentCapabilities, AuthMethod, AuthMethodAgent, AuthMethodTerminal,
        AuthenticateRequest, AuthenticateResponse, CreateElicitationRequest,
        CreateElicitationResponse, ElicitationAction, ElicitationRequestScope, ElicitationUrlMode,
        InitializeRequest, InitializeResponse, LogoutCapabilities, LogoutRequest, LogoutResponse,
        NewSessionRequest, NewSessionResponse, SessionConfigOption, SessionConfigOptionValue,
        SessionMode, SessionModeState, SetSessionConfigOptionRequest,
        SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse,
    },
};
use agent_client_protocol::{Agent, Channel, Error};
use tokio::sync::mpsc;

async fn until(rx: &EventRx, predicate: impl Fn(&BridgeEvent) -> bool) -> Vec<BridgeEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        if let Some(batch) = rx.drain() {
            let mut found = false;
            for event in batch {
                found |= predicate(&event);
                seen.push(event);
            }
            if found {
                return seen;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "event not received: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn set_mode_and_boolean_config_emit_successful_state() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |request: InitializeRequest, responder, _cx| {
                assert!(
                    request
                        .client_capabilities
                        .session
                        .unwrap()
                        .config_options
                        .unwrap()
                        .boolean
                        .is_some()
                );
                responder.respond(InitializeResponse::new(ProtocolVersion::V1))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_: NewSessionRequest, responder, _cx| {
                responder.respond(
                    NewSessionResponse::new("s")
                        .modes(SessionModeState::new(
                            "normal",
                            vec![
                                SessionMode::new("normal", "Normal"),
                                SessionMode::new("plan", "Plan"),
                            ],
                        ))
                        .config_options(vec![SessionConfigOption::boolean(
                            "enabled", "Enabled", false,
                        )]),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: SetSessionModeRequest, responder, _cx| {
                assert_eq!(request.session_id.0.as_ref(), "s");
                assert_eq!(request.mode_id.0.as_ref(), "plan");
                responder.respond(SetSessionModeResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: SetSessionConfigOptionRequest, responder, _cx| {
                assert_eq!(request.session_id.0.as_ref(), "s");
                assert_eq!(request.config_id.0.as_ref(), "enabled");
                assert_eq!(request.value.as_bool(), Some(true));
                responder.respond(SetSessionConfigOptionResponse::new(vec![
                    SessionConfigOption::boolean("enabled", "Enabled", true),
                ]))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(server);
    let (wake, _) = wake_pair().unwrap();
    let (events, rx) = event_channel(wake);
    let (cmd, commands) = mpsc::unbounded_channel();
    let bridge = tokio::spawn(acp::run_with(
        client,
        SessionOptions::new(root.path().into()),
        events,
        commands,
    ));
    let agent = tokio::spawn(fake);
    until(&rx, |e| matches!(e, BridgeEvent::SessionStarted { .. })).await;
    cmd.send(UiCmd::SetMode("plan".into())).unwrap();
    until(
        &rx,
        |e| matches!(e, BridgeEvent::ModeChanged(mode) if mode.0.as_ref() == "plan"),
    )
    .await;
    cmd.send(UiCmd::SetConfigOption {
        config_id: "enabled".into(),
        value: SessionConfigOptionValue::boolean(true),
    })
    .unwrap();
    let events = until(&rx, |e| matches!(e, BridgeEvent::ConfigOptions(_))).await;
    assert!(
        matches!(events.last(), Some(BridgeEvent::ConfigOptions(options)) if matches!(&options[0].kind,
        agent_client_protocol::schema::v1::SessionConfigKind::Boolean(boolean) if boolean.current_value))
    );
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn terminal_auth_retry_and_logout_require_new_auth() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let ready = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let fake =
        Agent
            .builder()
            .on_receive_request(
                async |request: InitializeRequest, responder, _cx| {
                    assert!(request.client_capabilities.auth.terminal);
                    responder.respond(
                        InitializeResponse::new(ProtocolVersion::V1)
                            .auth_methods(vec![AuthMethod::Terminal(AuthMethodTerminal::new(
                                "login", "Login",
                            ))])
                            .agent_capabilities(AgentCapabilities::new().auth(
                                AgentAuthCapabilities::new().logout(LogoutCapabilities::new()),
                            )),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let ready = ready.clone();
                    let calls = calls.clone();
                    async move |_: NewSessionRequest, responder, _cx| {
                        let attempt = calls.fetch_add(1, Ordering::SeqCst);
                        if ready.load(Ordering::SeqCst) {
                            responder.respond(NewSessionResponse::new(format!("s-{attempt}")))
                        } else {
                            responder.respond_with_error(Error::auth_required())
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let ready = ready.clone();
                    async move |_: LogoutRequest, responder, _cx| {
                        ready.store(false, Ordering::SeqCst);
                        responder.respond(LogoutResponse::new())
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(server);
    let (wake, _) = wake_pair().unwrap();
    let (events, rx) = event_channel(wake);
    let (cmd, commands) = mpsc::unbounded_channel();
    let bridge = tokio::spawn(acp::run_with(
        client,
        SessionOptions::new(root.path().into()),
        events,
        commands,
    ));
    let agent = tokio::spawn(fake);
    let auth = until(&rx, |e| matches!(e, BridgeEvent::AuthRequired { .. })).await;
    assert!(
        matches!(auth.last(), Some(BridgeEvent::AuthRequired { methods, .. }) if matches!(&methods[0], AuthMethod::Terminal(_)))
    );
    ready.store(true, Ordering::SeqCst);
    cmd.send(UiCmd::RetrySession).unwrap();
    until(&rx, |e| matches!(e, BridgeEvent::SessionStarted { .. })).await;
    cmd.send(UiCmd::Logout).unwrap();
    let logged = until(&rx, |e| matches!(e, BridgeEvent::LoggedOut)).await;
    assert!(logged.iter().any(|e| matches!(e, BridgeEvent::LoggedOut)));
    if !logged
        .iter()
        .any(|e| matches!(e, BridgeEvent::AuthRequired { .. }))
    {
        until(&rx, |e| matches!(e, BridgeEvent::AuthRequired { .. })).await;
    }
    ready.store(true, Ordering::SeqCst);
    cmd.send(UiCmd::RetrySession).unwrap();
    until(&rx, |e| matches!(e, BridgeEvent::SessionStarted { .. })).await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn authentication_can_wait_for_request_scoped_url_elicitation() {
    use agent_client_protocol::{Client, ConnectionTo};
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let ready = Arc::new(AtomicBool::new(false));
    let fake =
        Agent
            .builder()
            .on_receive_request(
                async |_: InitializeRequest, responder, _cx| {
                    responder.respond(InitializeResponse::new(ProtocolVersion::V1).auth_methods(
                        vec![AuthMethod::Agent(AuthMethodAgent::new("login", "Login"))],
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let ready = ready.clone();
                    async move |_: NewSessionRequest, responder, _cx| {
                        if ready.load(Ordering::SeqCst) {
                            responder.respond(NewSessionResponse::new("ready"))
                        } else {
                            responder.respond_with_error(Error::auth_required())
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let ready = ready.clone();
                    async move |_: AuthenticateRequest, responder, cx: ConnectionTo<Client>| {
                        let worker = cx.clone();
                        let ready = ready.clone();
                        cx.spawn(async move {
                            let answer = worker
                                .send_request(CreateElicitationRequest::new(
                                    ElicitationUrlMode::new(
                                        ElicitationRequestScope::new(1),
                                        "device-code",
                                        "https://example.invalid/login",
                                    ),
                                    "Log in",
                                ))
                                .block_task()
                                .await?;
                            assert_eq!(answer.action, ElicitationAction::Decline);
                            ready.store(true, Ordering::SeqCst);
                            responder.respond(AuthenticateResponse::new())
                        })?;
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(server);
    let (wake, _) = wake_pair().unwrap();
    let (events, rx) = event_channel(wake);
    let (cmd, commands) = mpsc::unbounded_channel();
    let bridge = tokio::spawn(acp::run_with(
        client,
        SessionOptions::new(root.path().into()),
        events,
        commands,
    ));
    let agent = tokio::spawn(fake);
    until(&rx, |e| matches!(e, BridgeEvent::AuthRequired { .. })).await;
    cmd.send(UiCmd::Authenticate("login".into())).unwrap();
    let dialog = until(&rx, |e| matches!(e, BridgeEvent::Elicitation { .. })).await;
    let id = match dialog.last().unwrap() {
        BridgeEvent::Elicitation { id, .. } => *id,
        _ => unreachable!(),
    };
    cmd.send(UiCmd::Elicitation {
        id,
        response: CreateElicitationResponse::new(ElicitationAction::Decline),
    })
    .unwrap();
    until(&rx, |e| matches!(e, BridgeEvent::SessionStarted { .. })).await;
    assert!(ready.load(Ordering::SeqCst));
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
