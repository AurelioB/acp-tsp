use std::time::{Duration, Instant};

use acp_tsp::{
    acp,
    bus::{EventRx, event_channel, wake_pair},
    event::{BridgeEvent, SessionStartKind, UiCmd},
    launch::SessionOptions,
};
#[cfg(feature = "unstable_session_fork")]
use agent_client_protocol::schema::v1::{
    ForkSessionRequest, ForkSessionResponse, SessionForkCapabilities,
};
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AgentCapabilities, InitializeRequest, InitializeResponse, LoadSessionRequest,
        LoadSessionResponse, NewSessionRequest, NewSessionResponse, ResumeSessionRequest,
        ResumeSessionResponse, SessionCapabilities, SessionResumeCapabilities,
    },
};
use agent_client_protocol::{Agent, Channel};
use tokio::sync::mpsc;

async fn started(rx: &EventRx, kind: SessionStartKind) {
    let start = Instant::now();
    loop {
        if let Some(batch) = rx.drain() {
            for event in batch {
                assert!(
                    !matches!(event, BridgeEvent::Error(_)),
                    "setup failed: {event:?}"
                );
                if matches!(event, BridgeEvent::SessionStarted {kind: found, ..} if found == kind) {
                    return;
                }
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "session setup stalled for {kind:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn all_session_setup_methods_carry_configured_meta() {
    let cwd = tempfile::tempdir().unwrap();
    let metadata =
        serde_json::json!({"claudeCode":{"options":{"allowDangerouslySkipPermissions":false}}});
    let expected = metadata.as_object().unwrap().clone();
    let (client, server) = Channel::duplex();
    let builder = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                let session = SessionCapabilities::new().resume(SessionResumeCapabilities::new());
                #[cfg(feature = "unstable_session_fork")]
                let session = session.fork(SessionForkCapabilities::new());
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new()
                            .load_session(true)
                            .session_capabilities(session),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let expected = expected.clone();
                async move |request: NewSessionRequest, responder, _cx| {
                    assert_eq!(request.meta, Some(expected.clone()));
                    responder.respond(NewSessionResponse::new("original"))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let expected = expected.clone();
                async move |request: LoadSessionRequest, responder, _cx| {
                    assert_eq!(request.meta, Some(expected.clone()));
                    responder.respond(LoadSessionResponse::new())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let expected = expected.clone();
                async move |request: ResumeSessionRequest, responder, _cx| {
                    assert_eq!(request.meta, Some(expected.clone()));
                    responder.respond(ResumeSessionResponse::new())
                }
            },
            agent_client_protocol::on_receive_request!(),
        );
    #[cfg(feature = "unstable_session_fork")]
    let builder = builder.on_receive_request(
        {
            let expected = expected.clone();
            async move |request: ForkSessionRequest, responder, _cx| {
                assert_eq!(request.meta, Some(expected.clone()));
                responder.respond(ForkSessionResponse::new("forked"))
            }
        },
        agent_client_protocol::on_receive_request!(),
    );
    let fake = builder.connect_to(server);
    let (wake, _) = wake_pair().unwrap();
    let (events, rx) = event_channel(wake);
    let (cmd, commands) = mpsc::unbounded_channel();
    let mut options = SessionOptions::new(cwd.path().into());
    options.session_meta = Some(expected);
    let bridge = tokio::spawn(acp::run_with(client, options, events, commands));
    let agent = tokio::spawn(fake);
    started(&rx, SessionStartKind::New).await;
    cmd.send(UiCmd::LoadSession("original".into())).unwrap();
    started(&rx, SessionStartKind::Load).await;
    cmd.send(UiCmd::ResumeSession("original".into())).unwrap();
    started(&rx, SessionStartKind::Resume).await;
    #[cfg(feature = "unstable_session_fork")]
    {
        cmd.send(UiCmd::ForkSession).unwrap();
        started(&rx, SessionStartKind::Fork).await;
    }
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
