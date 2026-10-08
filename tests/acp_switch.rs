use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use acp_tsp::{
    acp,
    bus::{EventRx, event_channel, wake_pair},
    event::{BridgeEvent, SessionStartKind, UiCmd},
    launch::SessionOptions,
};
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AgentCapabilities, CancelNotification, ContentBlock, CreateElicitationRequest,
        DeleteSessionRequest, DeleteSessionResponse, ElicitationAction, ElicitationFormMode,
        ElicitationSchema, ElicitationSessionScope, InitializeRequest, InitializeResponse,
        NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionCapabilities,
        SessionDeleteCapabilities, StopReason, TextContent,
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
async fn switching_sessions_cancels_turn_and_queued_prompts_before_switch() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let sessions = Arc::new(AtomicUsize::new(0));
    let prompts = Arc::new(AtomicUsize::new(0));
    let cancellations = Arc::new(AtomicUsize::new(0));
    let fake = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new().session_capabilities(
                            SessionCapabilities::new().delete(SessionDeleteCapabilities::new()),
                        ),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let sessions = sessions.clone();
                async move |_: NewSessionRequest, responder, _cx| {
                    let id = sessions.fetch_add(1, Ordering::SeqCst) + 1;
                    responder.respond(NewSessionResponse::new(format!("s-{id}")))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let cancellations = cancellations.clone();
                async move |_: CancelNotification, _cx| {
                    cancellations.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let prompts = prompts.clone();
                async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                    prompts.fetch_add(1, Ordering::SeqCst);
                    let worker = cx.clone();
                    cx.spawn(async move {
                        let answer = worker
                            .send_request(CreateElicitationRequest::new(
                                ElicitationFormMode::new(
                                    ElicitationSessionScope::new(request.session_id),
                                    ElicitationSchema::new(),
                                ),
                                "Confirm",
                            ))
                            .block_task()
                            .await?;
                        assert_eq!(answer.action, ElicitationAction::Cancel);
                        responder.respond(PromptResponse::new(StopReason::Cancelled))
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: DeleteSessionRequest, responder, _cx| {
                assert_eq!(request.session_id.0.as_ref(), "s-2");
                responder.respond(DeleteSessionResponse::new())
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
    until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { .. })
    })
    .await;

    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "one",
    ))]))
    .unwrap();
    let dialog = until(&rx, |event| {
        matches!(event, BridgeEvent::Elicitation { .. })
    })
    .await;
    let first_id = match dialog.last().unwrap() {
        BridgeEvent::Elicitation { id, .. } => *id,
        _ => unreachable!(),
    };
    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "queued",
    ))]))
    .unwrap();
    cmd.send(UiCmd::NewSession).unwrap();
    let transition = until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { session_id, .. }
        if session_id.0.as_ref() == "s-2")
    })
    .await;
    let resolved = transition
        .iter()
        .position(|event| {
            matches!(event, BridgeEvent::Resolved(id)
        if *id == first_id)
        })
        .unwrap();
    let ended = transition
        .iter()
        .position(|event| {
            matches!(event, BridgeEvent::TurnEnded(Ok(response))
        if response.stop_reason == StopReason::Cancelled)
        })
        .unwrap();
    let starting = transition
        .iter()
        .position(|event| matches!(event, BridgeEvent::SessionStarting(SessionStartKind::New)))
        .unwrap();
    assert!(resolved < ended && ended < starting);
    assert_eq!(
        prompts.load(Ordering::SeqCst),
        1,
        "queued prompt ran in the old session"
    );

    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "two",
    ))]))
    .unwrap();
    let dialog = until(&rx, |event| {
        matches!(event, BridgeEvent::Elicitation { .. })
    })
    .await;
    let second_id = match dialog.last().unwrap() {
        BridgeEvent::Elicitation { id, .. } => *id,
        _ => unreachable!(),
    };
    cmd.send(UiCmd::DeleteSession("s-2".into())).unwrap();
    let transition = until(&rx, |event| matches!(event, BridgeEvent::SessionDeleted(_))).await;
    let resolved = transition
        .iter()
        .position(|event| {
            matches!(event, BridgeEvent::Resolved(id)
        if *id == second_id)
        })
        .unwrap();
    let ended = transition
        .iter()
        .position(|event| matches!(event, BridgeEvent::TurnEnded(_)))
        .unwrap();
    let closed = transition
        .iter()
        .position(|event| matches!(event, BridgeEvent::SessionClosed))
        .unwrap();
    assert!(resolved < ended && ended < closed);
    assert_eq!(cancellations.load(Ordering::SeqCst), 2);
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
