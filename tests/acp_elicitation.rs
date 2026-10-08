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
        AgentCapabilities, ContentBlock, CreateElicitationRequest, CreateElicitationResponse,
        ElicitationAcceptAction, ElicitationAction, ElicitationFormMode, ElicitationSchema,
        ElicitationSessionScope, InitializeRequest, InitializeResponse, NewSessionRequest,
        NewSessionResponse, PromptRequest, PromptResponse, StopReason, TextContent,
    },
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, Error};
use tokio::sync::mpsc;

async fn until(rx: &EventRx, matches: impl Fn(&BridgeEvent) -> bool) -> Vec<BridgeEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        if let Some(batch) = rx.drain() {
            for event in batch {
                let found = matches(&event);
                seen.push(event);
                if found {
                    return seen;
                }
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "event not received: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn elicit() -> CreateElicitationRequest {
    CreateElicitationRequest::new(
        ElicitationFormMode::new(ElicitationSessionScope::new("s"), ElicitationSchema::new()),
        "Confirm",
    )
}

#[tokio::test]
async fn form_accept_cancel_and_peer_request_cancel_resolve_dialogs() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let (answer, mut answers) = mpsc::unbounded_channel();
    let fake = Agent.builder()
        .on_receive_request(async |_: InitializeRequest, responder, _cx| {
            responder.respond(InitializeResponse::new(ProtocolVersion::V1))
        }, agent_client_protocol::on_receive_request!())
        .on_receive_request(async |_: NewSessionRequest, responder, _cx| {
            responder.respond(NewSessionResponse::new("s"))
        }, agent_client_protocol::on_receive_request!())
        .on_receive_request({
            async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                let answer = answer.clone();
                let worker = cx.clone();
                cx.spawn(async move {
                    let result = if matches!(&request.prompt[0], ContentBlock::Text(t) if t.text == "peer-cancel") {
                        let pending = worker.send_request(elicit());
                        pending.cancel()?;
                        pending.block_task().await.map(|r| r.action)
                    } else {
                        worker.send_request(elicit()).block_task().await.map(|r| r.action)
                    };
                    let cancelled = matches!(&result, Err(err) if err.code == Error::request_cancelled().code);
                    answer.send((result.ok(), cancelled)).unwrap();
                    responder.respond(PromptResponse::new(StopReason::EndTurn))
                })?;
                Ok(())
            }
        }, agent_client_protocol::on_receive_request!())
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
    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "accept",
    ))]))
    .unwrap();
    let events = until(&rx, |e| matches!(e, BridgeEvent::Elicitation { .. })).await;
    let id = match events.last().unwrap() {
        BridgeEvent::Elicitation { id, .. } => *id,
        _ => unreachable!(),
    };
    cmd.send(UiCmd::Elicitation {
        id,
        response: CreateElicitationResponse::new(ElicitationAction::Accept(
            ElicitationAcceptAction::new(),
        )),
    })
    .unwrap();
    assert!(matches!(
        answers.recv().await.unwrap(),
        (Some(ElicitationAction::Accept(_)), false)
    ));
    until(&rx, |e| matches!(e, BridgeEvent::TurnEnded(_))).await;
    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "cancel",
    ))]))
    .unwrap();
    let events = until(&rx, |e| matches!(e, BridgeEvent::Elicitation { .. })).await;
    let id = match events.last().unwrap() {
        BridgeEvent::Elicitation { id, .. } => *id,
        _ => unreachable!(),
    };
    cmd.send(UiCmd::Cancel).unwrap();
    let events = until(&rx, |e| matches!(e, BridgeEvent::Resolved(n) if *n == id)).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, BridgeEvent::Resolved(n) if *n == id))
    );
    assert!(matches!(
        answers.recv().await.unwrap(),
        (Some(ElicitationAction::Cancel), false)
    ));
    // A reply with a stale id must not answer the next request.
    cmd.send(UiCmd::Elicitation {
        id,
        response: CreateElicitationResponse::new(ElicitationAction::Decline),
    })
    .unwrap();
    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "peer-cancel",
    ))]))
    .unwrap();
    let (result, cancelled) = tokio::time::timeout(Duration::from_secs(8), answers.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_none() && cancelled);
    let peer_events = until(&rx, |e| matches!(e, BridgeEvent::TurnEnded(_))).await;
    for event in &peer_events {
        if let BridgeEvent::Elicitation { id, .. } = event {
            assert!(
                peer_events
                    .iter()
                    .any(|e| matches!(e, BridgeEvent::Resolved(resolved) if resolved == id))
            );
        }
    }
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn close_and_delete_require_agent_capabilities() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_: NewSessionRequest, responder, _cx| {
                responder.respond(NewSessionResponse::new("s"))
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
    cmd.send(UiCmd::CloseSession).unwrap();
    let first = until(
        &rx,
        |e| matches!(e, BridgeEvent::Error(message) if message.contains("session/close")),
    )
    .await;
    assert!(
        !first
            .iter()
            .any(|e| matches!(e, BridgeEvent::SessionClosed))
    );
    cmd.send(UiCmd::DeleteSession("s".into())).unwrap();
    let second = until(
        &rx,
        |e| matches!(e, BridgeEvent::Error(message) if message.contains("session/delete")),
    )
    .await;
    assert!(!second.iter().any(|e| matches!(
        e,
        BridgeEvent::SessionClosed | BridgeEvent::SessionDeleted(_)
    )));
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
