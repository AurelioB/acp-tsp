use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use acp_tsp::acp;
use acp_tsp::bus::{EventRx, event_channel, wake_pair};
use acp_tsp::event::{BridgeEvent, UiCmd};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthMethodAgent, AuthenticateRequest, AuthenticateResponse, CancelNotification,
    ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
    ReadTextFileRequest, RequestPermissionOutcome, RequestPermissionRequest,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, StopReason, TextContent,
    ToolCallUpdate, ToolCallUpdateFields, WriteTextFileRequest,
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, Error};
use tokio::sync::mpsc;

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text(TextContent::new(value))]
}

// EventRx::drain drains the entire queue; collect a burst before inspecting it.
async fn until(rx: &EventRx, matches: impl Fn(&BridgeEvent) -> bool) -> Vec<BridgeEvent> {
    let start = Instant::now();
    let mut observed = Vec::new();
    loop {
        if let Some(events) = rx.drain() {
            for item in events {
                let found = matches(&item);
                observed.push(item);
                if found {
                    return observed;
                }
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "event not received: {observed:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn streams_chunks_then_ends_turn_and_queues_second_prompt() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let (second_release_tx, second_release_rx) = tokio::sync::oneshot::channel::<()>();
    let second_release = Arc::new(tokio::sync::Mutex::new(Some(second_release_rx)));
    let turns = Arc::new(AtomicUsize::new(0));
    let fake = Agent
        .builder()
        .on_receive_request(
            async |request: InitializeRequest, responder, _cx| {
                assert_eq!(request.protocol_version, ProtocolVersion::V1);
                assert!(request.client_capabilities.fs.read_text_file);
                assert!(request.client_capabilities.fs.write_text_file);
                assert!(!request.client_capabilities.terminal);
                responder.respond(InitializeResponse::new(ProtocolVersion::V1))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: NewSessionRequest, responder, _cx| {
                assert!(request.cwd.is_absolute());
                assert!(request.mcp_servers.is_empty());
                responder.respond(NewSessionResponse::new("s"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let release = release.clone();
                let turns = turns.clone();
                let second_tx = second_tx.clone();
                let second_release = second_release.clone();
                async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                    let turns = turns.clone();
                    let release = release.clone();
                    let second_tx = second_tx.clone();
                    let second_release = second_release.clone();
                    let turn = turns.fetch_add(1, Ordering::SeqCst);
                    cx.spawn({
                        let cx = cx.clone();
                        let release = release.clone();
                        let second_release = second_release.clone();
                        async move {
                            if turn == 0 {
                                for chunk in ["hello", " world"] {
                                    cx.send_notification(SessionNotification::new(
                                        request.session_id.clone(),
                                        SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                            ContentBlock::Text(TextContent::new(chunk)),
                                        )),
                                    ))?;
                                }
                                cx.send_notification(SessionNotification::new(
                                    "foreign",
                                    SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                        ContentBlock::Text(TextContent::new("not this session")),
                                    )),
                                ))?;
                                if let Some(wait) = release.lock().await.take() {
                                    let _ = wait.await;
                                }
                            } else {
                                second_tx.send(request.prompt).unwrap();
                                if let Some(wait) = second_release.lock().await.take() {
                                    let _ = wait.await;
                                }
                            }
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        }
                    })?;
                    Ok(())
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
        root.path().to_path_buf(),
        tx,
        commands,
    ));
    let agent = tokio::spawn(fake);
    let start = until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { .. })
    })
    .await;
    assert!(matches!(
        start.as_slice(),
        [
            BridgeEvent::Status(_),
            BridgeEvent::Initialized(_),
            BridgeEvent::Status(_),
            BridgeEvent::SessionStarted { .. }
        ]
    ));
    cmd.send(UiCmd::Prompt(text("first"))).unwrap();
    cmd.send(UiCmd::Prompt(text("second"))).unwrap();
    let chunks = until(&rx, |event| matches!(event, BridgeEvent::Update(update) if matches!(&**update, SessionUpdate::AgentMessageChunk(chunk) if matches!(&chunk.content, ContentBlock::Text(t) if t.text == " world")))).await;
    assert_eq!(chunks.len(), 2);
    assert_eq!(turns.load(Ordering::SeqCst), 1);
    release_tx.send(()).unwrap();
    let turns_ended = until(&rx, |event| {
        matches!(event, BridgeEvent::TurnEnded(Ok(StopReason::EndTurn)))
    })
    .await;
    assert!(matches!(
        turns_ended[0],
        BridgeEvent::TurnEnded(Ok(StopReason::EndTurn))
    ));
    assert!(
        matches!(second_rx.recv().await.unwrap().as_slice(), [ContentBlock::Text(t)] if t.text == "second")
    );
    assert_eq!(turns.load(Ordering::SeqCst), 2);
    second_release_tx.send(()).unwrap();
    until(&rx, |event| {
        matches!(event, BridgeEvent::TurnEnded(Ok(StopReason::EndTurn)))
    })
    .await;
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn permission_selection_and_cancel_resolve_pending_and_return_cancelled() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let (selected_tx, mut selected_rx) = mpsc::unbounded_channel();
    let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel();
    let cancelled = Arc::new(AtomicBool::new(false));
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
        .on_receive_notification(
            {
                let cancelled = cancelled.clone();
                async move |_: CancelNotification, _cx| {
                    cancelled.store(true, Ordering::SeqCst);
                    cancel_tx.send(()).unwrap();
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                let selected_tx = selected_tx.clone();
                let cancelled = cancelled.clone();
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
                        let response = cx.send_request(permission).block_task().await?;
                        selected_tx.send(response.outcome).unwrap();
                        let stop = if cancelled.load(Ordering::SeqCst) {
                            StopReason::Cancelled
                        } else {
                            StopReason::EndTurn
                        };
                        responder.respond(PromptResponse::new(stop))
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
    let bridge = tokio::spawn(acp::run_with(
        client,
        root.path().to_path_buf(),
        tx,
        commands,
    ));
    let agent = tokio::spawn(fake);
    until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { .. })
    })
    .await;
    cmd.send(UiCmd::Prompt(text("allow"))).unwrap();
    let mut events = until(&rx, |event| matches!(event, BridgeEvent::Permission { .. })).await;
    let BridgeEvent::Permission { id, .. } = events.pop().unwrap() else {
        unreachable!()
    };
    cmd.send(UiCmd::Permission {
        id,
        outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("yes")),
    })
    .unwrap();
    assert!(
        matches!(selected_rx.recv().await.unwrap(), RequestPermissionOutcome::Selected(selected) if selected.option_id.0.as_ref() == "yes")
    );
    until(&rx, |event| {
        matches!(event, BridgeEvent::TurnEnded(Ok(StopReason::EndTurn)))
    })
    .await;
    cmd.send(UiCmd::Prompt(text("cancel"))).unwrap();
    let mut events = until(&rx, |event| matches!(event, BridgeEvent::Permission { .. })).await;
    let BridgeEvent::Permission { id, .. } = events.pop().unwrap() else {
        unreachable!()
    };
    cmd.send(UiCmd::Cancel).unwrap();
    tokio::time::timeout(Duration::from_secs(5), cancel_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        selected_rx.recv().await.unwrap(),
        RequestPermissionOutcome::Cancelled
    ));
    let ended = until(&rx, |event| {
        matches!(event, BridgeEvent::TurnEnded(Ok(StopReason::Cancelled)))
    })
    .await;
    assert!(
        ended
            .iter()
            .any(|event| matches!(event, BridgeEvent::Resolved(actual) if *actual == id))
    );
    cmd.send(UiCmd::Permission {
        id,
        outcome: RequestPermissionOutcome::Cancelled,
    })
    .unwrap();
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn authenticates_then_allows_confined_file_operations() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let cwd = root.path().to_path_buf();
    let other = outside.path().join("secret");
    std::fs::write(&other, "secret").unwrap();
    let (client, server) = Channel::duplex();
    let authed = Arc::new(AtomicBool::new(false));
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
                    let authed = authed.clone();
                    async move |_: NewSessionRequest, responder, _cx| {
                        if authed.load(Ordering::SeqCst) {
                            responder.respond(NewSessionResponse::new("s"))
                        } else {
                            responder.respond_with_error(Error::auth_required())
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let authed = authed.clone();
                    async move |request: AuthenticateRequest, responder, _cx| {
                        assert_eq!(request.method_id.0.as_ref(), "login");
                        authed.store(true, Ordering::SeqCst);
                        responder.respond(AuthenticateResponse::new())
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_: PromptRequest, responder, cx: ConnectionTo<Client>| {
                    let cwd = cwd.clone();
                    let other = other.clone();
                    cx.spawn({
                        let cx = cx.clone();
                        async move {
                            let path = cwd.join("new/sub/file");
                            cx.send_request(WriteTextFileRequest::new(
                                "s",
                                &path,
                                "one\n two\nthree\n",
                            ))
                            .block_task()
                            .await
                            .expect("write within cwd");
                            let read = cx
                                .send_request(ReadTextFileRequest::new("s", &path).line(2).limit(1))
                                .block_task()
                                .await
                                .expect("read within cwd");
                            assert_eq!(read.content, " two\n");
                            let rejected = cx
                                .send_request(ReadTextFileRequest::new("s", &other))
                                .block_task()
                                .await
                                .unwrap_err();
                            assert_eq!(rejected.code, Error::invalid_params().code);
                            let rejected = cx
                                .send_request(WriteTextFileRequest::new("s", &other, "bad"))
                                .block_task()
                                .await
                                .unwrap_err();
                            assert_eq!(rejected.code, Error::invalid_params().code);
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
    let bridge = tokio::spawn(acp::run_with(
        client,
        root.path().to_path_buf(),
        tx,
        commands,
    ));
    let agent = tokio::spawn(fake);
    let auth = until(&rx, |event| {
        matches!(event, BridgeEvent::AuthRequired { .. })
    })
    .await;
    assert!(
        matches!(auth.last().unwrap(), BridgeEvent::AuthRequired { methods, .. } if methods.len() == 1)
    );
    cmd.send(UiCmd::Authenticate("login".into())).unwrap();
    until(&rx, |event| {
        matches!(event, BridgeEvent::SessionStarted { .. })
    })
    .await;
    cmd.send(UiCmd::Prompt(text("files"))).unwrap();
    until(&rx, |event| {
        matches!(event, BridgeEvent::TurnEnded(Ok(StopReason::EndTurn)))
    })
    .await;
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
