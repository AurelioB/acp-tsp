use std::path::PathBuf;
use std::time::{Duration, Instant};

use acp_tsp::acp;
use acp_tsp::bus::{EventRx, event_channel, wake_pair};
use acp_tsp::event::{BridgeEvent, SessionStartKind, UiCmd};
use acp_tsp::launch::{SessionOptions, StartSession};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, ContentChunk, CreateTerminalRequest, InitializeRequest,
    InitializeResponse, KillTerminalRequest, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, ReleaseTerminalRequest, SessionCapabilities, SessionInfo,
    SessionListCapabilities, SessionNotification, SessionUpdate, StopReason, TerminalOutputRequest,
    TextContent, WaitForTerminalExitRequest,
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, Error};
use tokio::sync::mpsc;

async fn until(rx: &EventRx, predicate: impl Fn(&BridgeEvent) -> bool) -> Vec<BridgeEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        if let Some(events) = rx.drain() {
            for event in events {
                let found = predicate(&event);
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

fn wire() -> (
    acp_tsp::bus::EventTx,
    EventRx,
    mpsc::UnboundedSender<UiCmd>,
    mpsc::UnboundedReceiver<UiCmd>,
) {
    let (wake, _) = wake_pair().unwrap();
    let (events, rx) = event_channel(wake);
    let (cmd, commands) = mpsc::unbounded_channel();
    (events, rx, cmd, commands)
}

#[tokio::test]
async fn terminal_request_lifecycle_and_cwd_confinement() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root_path = root.path().to_path_buf();
    let outside_path = outside.path().to_path_buf();
    let (client, server) = Channel::duplex();
    let (results, mut received) = mpsc::unbounded_channel();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |request: InitializeRequest, responder, _cx| {
                assert!(request.client_capabilities.terminal);
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
            {
                async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                    let results = results.clone();
                    let outside = outside_path.clone();
                    let worker = cx.clone();
                    cx.spawn(async move {
                        let invalid = worker
                            .send_request(CreateTerminalRequest::new("s", "sh").cwd(outside))
                            .block_task()
                            .await
                            .unwrap_err();
                        assert_eq!(invalid.code, Error::invalid_params().code);
                        let id = worker
                            .send_request(
                                CreateTerminalRequest::new("s", "sh")
                                    .args(vec!["-c".into(), "printf 'aé🌍z'; exit 7".into()])
                                    .output_byte_limit(5),
                            )
                            .block_task()
                            .await?
                            .terminal_id;
                        let exit = worker
                            .send_request(WaitForTerminalExitRequest::new("s", id.clone()))
                            .block_task()
                            .await?;
                        assert_eq!(exit.exit_status.exit_code, Some(7));
                        let output = worker
                            .send_request(TerminalOutputRequest::new("s", id.clone()))
                            .block_task()
                            .await?;
                        assert_eq!(output.output, "🌍z");
                        assert!(output.truncated);
                        assert_eq!(output.exit_status.unwrap().exit_code, Some(7));
                        worker
                            .send_request(ReleaseTerminalRequest::new("s", id.clone()))
                            .block_task()
                            .await?;
                        let err = worker
                            .send_request(TerminalOutputRequest::new("s", id))
                            .block_task()
                            .await
                            .unwrap_err();
                        assert_eq!(err.code, Error::invalid_params().code);
                        let live = worker
                            .send_request(
                                CreateTerminalRequest::new("s", "sh")
                                    .args(vec!["-c".into(), "sleep 30".into()]),
                            )
                            .block_task()
                            .await?
                            .terminal_id;
                        worker
                            .send_request(KillTerminalRequest::new("s", live.clone()))
                            .block_task()
                            .await?;
                        let status = worker
                            .send_request(WaitForTerminalExitRequest::new("s", live.clone()))
                            .block_task()
                            .await?;
                        assert!(status.exit_status.signal.is_some());
                        worker
                            .send_request(ReleaseTerminalRequest::new("s", live))
                            .block_task()
                            .await?;
                        results.send(()).unwrap();
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    })?;
                    let _ = request;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(server);
    let (events, rx, cmd, commands) = wire();
    let bridge = tokio::spawn(acp::run_with(
        client,
        SessionOptions::new(root_path),
        events,
        commands,
    ));
    let agent = tokio::spawn(fake);
    until(&rx, |e| matches!(e, BridgeEvent::SessionStarted { .. })).await;
    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "terminal",
    ))]))
    .unwrap();
    tokio::time::timeout(Duration::from_secs(8), received.recv())
        .await
        .unwrap()
        .unwrap();
    let events = until(&rx, |e| matches!(e, BridgeEvent::TurnEnded(_))).await;
    assert!(events.iter().any(|e| matches!(
        e,
        BridgeEvent::Terminal(acp_tsp::event::TerminalEvent::Created { .. })
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        BridgeEvent::Terminal(acp_tsp::event::TerminalEvent::Exited { .. })
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        BridgeEvent::Terminal(acp_tsp::event::TerminalEvent::Released(_))
    )));
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn load_replay_and_list_paging_filter_to_cwd() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().to_path_buf();
    let (client, server) = Channel::duplex();
    let agent = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new()
                            .load_session(true)
                            .session_capabilities(
                                SessionCapabilities::new().list(SessionListCapabilities::new()),
                            ),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_: NewSessionRequest, responder, _cx| {
                responder.respond(NewSessionResponse::new("fresh"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: LoadSessionRequest, responder, cx: ConnectionTo<Client>| {
                cx.send_notification(SessionNotification::new(
                    request.session_id,
                    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                        TextContent::new("replayed"),
                    ))),
                ))?;
                responder.respond(LoadSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: ListSessionsRequest, responder, _cx| {
                assert!(request.cwd.is_some());
                let cwd = request.cwd.unwrap();
                let page = if request.cursor.is_none() {
                    ListSessionsResponse::new(vec![
                        SessionInfo::new("first", cwd.clone()),
                        SessionInfo::new("other", PathBuf::from("/other")),
                    ])
                    .next_cursor("page-2")
                } else {
                    assert_eq!(request.cursor.as_deref(), Some("page-2"));
                    ListSessionsResponse::new(vec![SessionInfo::new("second", cwd)])
                };
                responder.respond(page)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(server);
    let (events, rx, cmd, commands) = wire();
    let mut options = SessionOptions::new(cwd);
    options.start = StartSession::Load("first".into());
    let bridge = tokio::spawn(acp::run_with(client, options, events, commands));
    let agent = tokio::spawn(agent);
    let startup = until(&rx, |e| {
        matches!(
            e,
            BridgeEvent::SessionStarted {
                kind: SessionStartKind::Load,
                ..
            }
        )
    })
    .await;
    let start = startup
        .iter()
        .position(|e| matches!(e, BridgeEvent::SessionStarting(SessionStartKind::Load)))
        .unwrap();
    let replay = startup
        .iter()
        .position(|e| matches!(e, BridgeEvent::Update(_)))
        .unwrap();
    assert!(
        start < replay && replay < startup.len() - 1,
        "replay order: {startup:?}"
    );
    cmd.send(UiCmd::ListSessions { cursor: None }).unwrap();
    let page = until(&rx, |e| {
        matches!(
            e,
            BridgeEvent::SessionList {
                first_page: true,
                ..
            }
        )
    })
    .await;
    assert!(
        matches!(page.last().unwrap(), BridgeEvent::SessionList { sessions, next_cursor: Some(cursor), .. }
        if sessions.len() == 1 && cursor == "page-2")
    );
    cmd.send(UiCmd::ListSessions {
        cursor: Some("page-2".into()),
    })
    .unwrap();
    let page = until(&rx, |e| {
        matches!(
            e,
            BridgeEvent::SessionList {
                first_page: false,
                ..
            }
        )
    })
    .await;
    assert!(
        matches!(page.last().unwrap(), BridgeEvent::SessionList { sessions, next_cursor: None, .. }
        if sessions.len() == 1 && sessions[0].session_id.0.as_ref() == "second")
    );
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn additional_directories_control_session_setup_and_fs_roots() {
    use agent_client_protocol::schema::v1::{
        McpCapabilities, McpServer, McpServerHttp, McpServerSse, McpServerStdio,
        ReadTextFileRequest, SessionAdditionalDirectoriesCapabilities,
    };
    for supported in [false, true] {
        let cwd = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let path = extra.path().join("hello.txt");
        std::fs::write(&path, "from extra").unwrap();
        let (client, server) = Channel::duplex();
        let (answer, mut received) = mpsc::unbounded_channel();
        let root = extra.path().to_path_buf();
        let fake = Agent
            .builder()
            .on_receive_request(
                async move |_: InitializeRequest, responder, _cx| {
                    let mut session = SessionCapabilities::new();
                    if supported {
                        session =
                            session.additional_directories(
                                SessionAdditionalDirectoriesCapabilities::new(),
                            );
                    }
                    responder.respond(
                        InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                            AgentCapabilities::new()
                                .session_capabilities(session)
                                .mcp_capabilities(McpCapabilities::new().http(true)),
                        ),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    async move |request: NewSessionRequest, responder, _cx| {
                        assert_eq!(request.additional_directories.len(), usize::from(supported));
                        assert!(
                            request.additional_directories.is_empty()
                                || request.additional_directories == [root.clone()]
                        );
                        assert_eq!(request.mcp_servers.len(), 2, "stdio + http, but not SSE");
                        responder.respond(NewSessionResponse::new("s"))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    async move |_: PromptRequest, responder, cx: ConnectionTo<Client>| {
                        let path = path.clone();
                        let answer = answer.clone();
                        let worker = cx.clone();
                        cx.spawn(async move {
                            let result = worker
                                .send_request(ReadTextFileRequest::new("s", path))
                                .block_task()
                                .await;
                            answer.send(result).unwrap();
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        })?;
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(server);
        let (events, rx, cmd, commands) = wire();
        let mut options = SessionOptions::new(cwd.path().into());
        options.additional_dirs.push(extra.path().into());
        options.mcp_servers = vec![
            McpServer::Stdio(McpServerStdio::new("local", "/bin/echo")),
            McpServer::Http(McpServerHttp::new("http", "https://example.test/mcp")),
            McpServer::Sse(McpServerSse::new("sse", "https://example.test/sse")),
        ];
        let bridge = tokio::spawn(acp::run_with(client, options, events, commands));
        let agent = tokio::spawn(fake);
        let startup = until(&rx, |e| matches!(e, BridgeEvent::SessionStarted { .. })).await;
        assert!(
            startup
                .iter()
                .any(|e| matches!(e, BridgeEvent::Error(message) if message.contains("MCP")))
        );
        assert_eq!(startup.iter().filter(|e| matches!(e, BridgeEvent::Error(message) if message.contains("additional directories"))).count(), usize::from(!supported));
        cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
            "read",
        ))]))
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(8), received.recv())
            .await
            .unwrap()
            .unwrap();
        if supported {
            assert_eq!(result.unwrap().content, "from extra");
        } else {
            assert_eq!(result.unwrap_err().code, Error::invalid_params().code);
        }
        cmd.send(UiCmd::Shutdown).unwrap();
        bridge.await.unwrap().unwrap();
        agent.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn continue_selects_newest_session_and_resumes_without_load_capability() {
    use agent_client_protocol::schema::v1::{
        ResumeSessionRequest, ResumeSessionResponse, SessionResumeCapabilities,
    };
    let cwd = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new().session_capabilities(
                            SessionCapabilities::new()
                                .list(SessionListCapabilities::new())
                                .resume(SessionResumeCapabilities::new()),
                        ),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: ListSessionsRequest, responder, _cx| {
                let root = request.cwd.unwrap();
                let response = if request.cursor.is_none() {
                    ListSessionsResponse::new(vec![
                        SessionInfo::new("old", root.clone()).updated_at("2025-01-01T00:00:00Z"),
                    ])
                    .next_cursor("next")
                } else {
                    ListSessionsResponse::new(vec![
                        SessionInfo::new("other", "/different").updated_at("2099-01-01T00:00:00Z"),
                        SessionInfo::new("newest", root).updated_at("2026-01-01T00:00:00Z"),
                    ])
                };
                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: ResumeSessionRequest, responder, _cx| {
                assert_eq!(request.session_id.0.as_ref(), "newest");
                responder.respond(ResumeSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(server);
    let (events, rx, cmd, commands) = wire();
    let mut options = SessionOptions::new(cwd.path().into());
    options.start = StartSession::Latest;
    let bridge = tokio::spawn(acp::run_with(client, options, events, commands));
    let agent = tokio::spawn(fake);
    let events = until(&rx, |e| matches!(e, BridgeEvent::SessionStarted { .. })).await;
    assert!(
        matches!(events.last(), Some(BridgeEvent::SessionStarted { kind: SessionStartKind::Resume, session_id, .. })
        if session_id.0.as_ref() == "newest")
    );
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
