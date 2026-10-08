use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use acp_tsp::{
    acp,
    bus::{EventRx, event_channel, wake_pair},
    event::{BridgeEvent, UiCmd},
    launch::SessionOptions,
};
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
        PromptRequest, PromptResponse, StopReason, TextContent,
    },
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, Error, UntypedMessage};
use tokio::sync::{Notify, mpsc};

async fn until(rx: &EventRx, predicate: impl Fn(&BridgeEvent) -> bool) -> Vec<BridgeEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        if let Some(batch) = rx.drain() {
            for event in batch {
                let found = predicate(&event);
                seen.push(event);
                if found {
                    return seen;
                }
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "missing event: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn text(message: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text(TextContent::new(message))]
}

#[tokio::test]
async fn extensions_forward_notifications_and_return_request_results() {
    let cwd = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |request: InitializeRequest, responder, cx: ConnectionTo<Client>| {
                let meta = request.client_capabilities.meta.as_ref().unwrap();
                assert_eq!(meta.get("terminal_output"), Some(&serde_json::json!(true)));
                assert_eq!(meta.get("terminal-auth"), Some(&serde_json::json!(true)));
                assert!(!meta.contains_key("jetbrains"));
                cx.send_notification(UntypedMessage::new(
                    "_auth/status_update",
                    serde_json::json!({"status":"ready"}),
                )?)?;
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
            async |message: UntypedMessage, responder, _cx| {
                assert_eq!(message.method, "_session/goal");
                responder.respond(serde_json::json!({"accepted": message.params["objective"]}))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(server);
    let (wake, _) = wake_pair().unwrap();
    let (tx, rx) = event_channel(wake);
    let (cmd, commands) = mpsc::unbounded_channel();
    let bridge = tokio::spawn(acp::run_with(
        client,
        SessionOptions::new(cwd.path().into()),
        tx,
        commands,
    ));
    let agent = tokio::spawn(fake);
    let mut received = until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { .. })
    })
    .await;
    if !received
        .iter()
        .any(|event| matches!(event, BridgeEvent::Extension { .. }))
    {
        received.extend(until(&rx, |event| matches!(event, BridgeEvent::Extension { .. })).await);
    }
    assert!(received.iter().any(|event| matches!(event, BridgeEvent::Extension { method, params } if method == "_auth/status_update" && params["status"] == "ready")));
    cmd.send(UiCmd::Extension {
        method: "_session/goal".into(),
        params: serde_json::json!({"objective":"ship"}),
    })
    .unwrap();
    let result = until(&rx, |event| {
        matches!(event, BridgeEvent::ExtensionResult { .. })
    })
    .await;
    assert!(result.iter().any(|event| matches!(event, BridgeEvent::ExtensionResult { method, result: Ok(value) } if method == "_session/goal" && value["accepted"] == "ship")));
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn steer_injects_supported_turn_and_falls_back_on_failure_and_idle() {
    for (supported, steer_fails) in [(true, false), (true, true), (false, false)] {
        let cwd = tempfile::tempdir().unwrap();
        let (client, server) = Channel::duplex();
        let turns = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());
        let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel();
        let (steer_tx, mut steer_rx) = mpsc::unbounded_channel();
        let fake = Agent
            .builder()
            .on_receive_request(
                async move |_: InitializeRequest, responder, _cx| {
                    let response = InitializeResponse::new(ProtocolVersion::V1);
                    responder.respond(if supported {
                        response.meta(
                            serde_json::json!({"steering":{"supported":true}})
                                .as_object()
                                .cloned(),
                        )
                    } else {
                        response
                    })
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
                {
                    let turns = turns.clone();
                    let release = release.clone();
                    async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                        let turn = turns.fetch_add(1, Ordering::SeqCst);
                        prompt_tx.send(request.prompt).unwrap();
                        let release = release.clone();
                        cx.spawn(async move {
                            if turn == 0 {
                                release.notified().await;
                            }
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        })?;
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: UntypedMessage, responder, _cx| {
                    assert_eq!(request.method, "_session/steering");
                    assert_eq!(request.params["sessionId"], "s");
                    assert_eq!(
                        request.params["_meta"]["steering"]["idleBehavior"],
                        "promptRequired"
                    );
                    steer_tx.send(request.params["prompt"].clone()).unwrap();
                    if steer_fails {
                        responder.respond_with_error(Error::invalid_request())
                    } else {
                        responder.respond(serde_json::json!({"outcome":"injected"}))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(server);
        let (wake, _) = wake_pair().unwrap();
        let (tx, rx) = event_channel(wake);
        let (cmd, commands) = mpsc::unbounded_channel();
        let bridge = tokio::spawn(acp::run_with(
            client,
            SessionOptions::new(cwd.path().into()),
            tx,
            commands,
        ));
        let agent = tokio::spawn(fake);
        until(&rx, |event| {
            matches!(event, BridgeEvent::SessionStarted { .. })
        })
        .await;
        cmd.send(UiCmd::Prompt(text("first"))).unwrap();
        assert_eq!(prompt_rx.recv().await.unwrap(), text("first"));
        cmd.send(UiCmd::Steer(text("steered"))).unwrap();
        if supported {
            assert_eq!(
                steer_rx.recv().await.unwrap(),
                serde_json::json!(text("steered"))
            );
        }
        release.notify_one();
        until(&rx, |event| matches!(event, BridgeEvent::TurnEnded(_))).await;
        if !supported || steer_fails {
            assert_eq!(prompt_rx.recv().await.unwrap(), text("steered"));
        } else {
            assert_eq!(turns.load(Ordering::SeqCst), 1);
            cmd.send(UiCmd::Steer(text("idle"))).unwrap();
            assert_eq!(prompt_rx.recv().await.unwrap(), text("idle"));
            assert_eq!(turns.load(Ordering::SeqCst), 2);
        }
        cmd.send(UiCmd::Shutdown).unwrap();
        bridge.await.unwrap().unwrap();
        agent.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn unknown_agent_extension_request_is_method_not_found() {
    let cwd = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let (result_tx, mut result_rx) = mpsc::unbounded_channel();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(InitializeResponse::new(ProtocolVersion::V1))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_: NewSessionRequest, responder, cx: ConnectionTo<Client>| {
                responder.respond(NewSessionResponse::new("s"))?;
                let worker = cx.clone();
                let result_tx = result_tx.clone();
                cx.spawn(async move {
                    let error = worker
                        .send_request(UntypedMessage::new(
                            "_agent/unsupported",
                            serde_json::json!({}),
                        )?)
                        .block_task()
                        .await
                        .unwrap_err();
                    result_tx.send(error.code).unwrap();
                    Ok(())
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(server);
    let (wake, _) = wake_pair().unwrap();
    let (tx, rx) = event_channel(wake);
    let (cmd, commands) = mpsc::unbounded_channel();
    let bridge = tokio::spawn(acp::run_with(
        client,
        SessionOptions::new(cwd.path().into()),
        tx,
        commands,
    ));
    let agent = tokio::spawn(fake);
    until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { .. })
    })
    .await;
    let code = tokio::time::timeout(Duration::from_secs(8), result_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code, Error::method_not_found().code);
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
