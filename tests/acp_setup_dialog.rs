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
        CreateElicitationRequest, CreateElicitationResponse, ElicitationAction,
        ElicitationRequestScope, ElicitationUrlMode, InitializeRequest, InitializeResponse,
        NewSessionRequest, NewSessionResponse,
    },
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo};
use tokio::sync::mpsc;

async fn until(rx: &EventRx, predicate: impl Fn(&BridgeEvent) -> bool) -> Vec<BridgeEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        if let Some(batch) = rx.drain() {
            let found = batch.iter().any(&predicate);
            seen.extend(batch);
            if found {
                return seen;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "missing event: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn session_setup_can_wait_for_url_elicitation() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(InitializeResponse::new(ProtocolVersion::V1))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_: NewSessionRequest, responder, cx: ConnectionTo<Client>| {
                let worker = cx.clone();
                cx.spawn(async move {
                    let answer = worker
                        .send_request(CreateElicitationRequest::new(
                            ElicitationUrlMode::new(
                                ElicitationRequestScope::new(1),
                                "setup-login",
                                "https://example.invalid/setup",
                            ),
                            "Authenticate",
                        ))
                        .block_task()
                        .await?;
                    assert_eq!(answer.action, ElicitationAction::Decline);
                    responder.respond(NewSessionResponse::new("ready"))
                })?;
                Ok(())
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
    let dialog = until(&rx, |event| {
        matches!(event, BridgeEvent::Elicitation { .. })
    })
    .await;
    let id = match dialog.last().unwrap() {
        BridgeEvent::Elicitation { id, .. } => *id,
        _ => unreachable!(),
    };
    cmd.send(UiCmd::Elicitation {
        id,
        response: CreateElicitationResponse::new(ElicitationAction::Decline),
    })
    .unwrap();
    let started = until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { .. })
    })
    .await;
    assert!(started.iter().any(|event| matches!(event,
        BridgeEvent::SessionStarted { session_id, .. } if session_id.0.as_ref() == "ready")));
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
