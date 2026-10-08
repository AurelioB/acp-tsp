use std::time::{Duration, Instant};

use acp_tsp::acp;
use acp_tsp::bus::{EventRx, event_channel, wake_pair};
use acp_tsp::event::{BridgeEvent, UiCmd};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, SelectedPermissionOutcome, StopReason,
    TextContent, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, Error};
use tokio::sync::mpsc;

async fn until(rx: &EventRx, matching: impl Fn(&BridgeEvent) -> bool) -> Vec<BridgeEvent> {
    let start = Instant::now();
    let mut events = Vec::new();
    loop {
        if let Some(batch) = rx.drain() {
            for event in batch {
                let found = matching(&event);
                events.push(event);
                if found {
                    return events;
                }
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "event timeout: {events:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn remote_request_cancellation_resolves_dialog_with_request_cancelled() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let (response_tx, mut response_rx) = mpsc::unbounded_channel();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(InitializeResponse::new(ProtocolVersion::V1))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_: NewSessionRequest, responder, _cx| {
                responder.respond(NewSessionResponse::new("s"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                let response_tx = response_tx.clone();
                cx.spawn({
                    let cx = cx.clone();
                    async move {
                        let permission = cx.send_request(RequestPermissionRequest::new(
                            request.session_id,
                            ToolCallUpdate::new("tool", ToolCallUpdateFields::new()),
                            vec![PermissionOption::new(
                                "yes",
                                "Allow",
                                PermissionOptionKind::AllowOnce,
                            )],
                        ));
                        // Let the UI see the pending request before cancelling it.
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        permission.cancel()?;
                        response_tx.send(permission.block_task().await).unwrap();
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    }
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(server);
    let (wake, _) = wake_pair().unwrap();
    let (tx, rx) = event_channel(wake);
    let (cmd, commands) = mpsc::unbounded_channel();
    let bridge = tokio::spawn(acp::run_with(client, root.path().into(), tx, commands));
    let agent = tokio::spawn(fake);
    until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { .. })
    })
    .await;
    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "cancel request",
    ))]))
    .unwrap();
    let events = until(&rx, |event| matches!(event, BridgeEvent::Permission { .. })).await;
    let BridgeEvent::Permission { id, .. } = events.last().unwrap() else {
        unreachable!()
    };
    let id = *id;
    assert_eq!(
        response_rx.recv().await.unwrap().unwrap_err().code,
        Error::request_cancelled().code
    );
    let events = until(&rx, |event| {
        matches!(event, BridgeEvent::TurnEnded(Ok(StopReason::EndTurn)))
    })
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, BridgeEvent::Resolved(actual) if *actual == id))
    );
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn restart_dismisses_old_permission_and_stale_answer_cannot_affect_new_connection() {
    let root = tempfile::tempdir().unwrap();
    let (wake, _) = wake_pair().unwrap();
    let (tx, rx) = event_channel(wake);
    let mut old_id = None;
    for generation in 0..2 {
        let (client, server) = Channel::duplex();
        let (answer_tx, mut answer_rx) = mpsc::unbounded_channel();
        let fake = Agent
            .builder()
            .on_receive_request(
                async |_: InitializeRequest, responder, _cx| {
                    responder.respond(InitializeResponse::new(ProtocolVersion::V1))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async |_: NewSessionRequest, responder, _cx| {
                    responder.respond(NewSessionResponse::new("s"))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                    let answer_tx = answer_tx.clone();
                    cx.spawn({
                        let cx = cx.clone();
                        async move {
                            let permission = RequestPermissionRequest::new(
                                request.session_id,
                                ToolCallUpdate::new("tool", ToolCallUpdateFields::new()),
                                vec![PermissionOption::new(
                                    "yes",
                                    "Allow",
                                    PermissionOptionKind::AllowOnce,
                                )],
                            );
                            let answer = cx.send_request(permission).block_task().await;
                            let _ = answer_tx.send(answer);
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        }
                    })?;
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(server);
        let (cmd, commands) = mpsc::unbounded_channel();
        let bridge = tokio::spawn(acp::run_with(
            client,
            root.path().into(),
            tx.clone(),
            commands,
        ));
        let agent = tokio::spawn(fake);
        until(&rx, |event| {
            matches!(event, BridgeEvent::SessionStarted { .. })
        })
        .await;
        if let Some(id) = old_id {
            cmd.send(UiCmd::Permission {
                id,
                outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("yes")),
            })
            .unwrap();
        }
        cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
            "permission",
        ))]))
        .unwrap();
        let events = until(&rx, |event| matches!(event, BridgeEvent::Permission { .. })).await;
        let BridgeEvent::Permission { id, .. } = events.last().unwrap() else {
            unreachable!()
        };
        let id = *id;
        if generation == 0 {
            old_id = Some(id);
            cmd.send(UiCmd::Restart).unwrap();
            bridge.await.unwrap().unwrap();
            let events = until(
                &rx,
                |event| matches!(event, BridgeEvent::Resolved(actual) if *actual == id),
            )
            .await;
            assert!(matches!(events.last(), Some(BridgeEvent::Resolved(actual)) if *actual == id));
        } else {
            assert_ne!(id, old_id.unwrap());
            assert!(
                tokio::time::timeout(Duration::from_millis(50), answer_rx.recv())
                    .await
                    .is_err()
            );
            cmd.send(UiCmd::Permission {
                id,
                outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("yes")),
            })
            .unwrap();
            assert!(matches!(
                answer_rx.recv().await.unwrap().unwrap().outcome,
                RequestPermissionOutcome::Selected(_)
            ));
            until(&rx, |event| {
                matches!(event, BridgeEvent::TurnEnded(Ok(StopReason::EndTurn)))
            })
            .await;
            cmd.send(UiCmd::Shutdown).unwrap();
            bridge.await.unwrap().unwrap();
        }
        let _ = agent.await;
    }
}
