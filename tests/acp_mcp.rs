#![cfg(feature = "unstable_mcp_over_acp")]
use parking_lot::Mutex;
use std::sync::Arc;
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
        AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
        McpCapabilities, McpServer, McpServerAcpId, MessageMcpRequest, MessageMcpResponse,
        NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionNotification,
        SessionUpdate, StopReason, TextContent,
    },
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo};
use serde_json::{Value, json};
use tokio::sync::mpsc;

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
            "event not received: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn call(id: McpServerAcpId, request_id: &str, method: &str, data: Value) -> MessageMcpRequest {
    let mut data = data.as_object().cloned().unwrap();
    data.insert(
        "_meta".into(),
        json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }),
    );
    MessageMcpRequest::new(id, request_id.to_owned(), method.to_owned()).params(data)
}

#[tokio::test]
async fn tern_open_tool_rejects_paths_outside_roots_and_preserves_session_updates() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("secret");
    std::fs::write(&outside_file, "hidden").unwrap();
    let (client, server) = Channel::duplex();
    let server_id: Arc<Mutex<Option<McpServerAcpId>>> = Arc::new(Mutex::new(None));
    let (done, mut receiver) = mpsc::unbounded_channel();
    let fake = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new().mcp_capabilities(McpCapabilities::new().acp(true)),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let server_id = server_id.clone();
                async move |request: NewSessionRequest, responder, _cx| {
                    let McpServer::Acp(server) = &request.mcp_servers[0] else {
                        panic!("missing Tern MCP server")
                    };
                    assert_eq!(server.name, "tern");
                    *server_id.lock() = Some(server.server_id.clone());
                    responder.respond(NewSessionResponse::new("s"))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let server_id = server_id.clone();
                async move |_: PromptRequest, responder, cx: ConnectionTo<Client>| {
                    let id = server_id.lock().clone().unwrap();
                    let outside = outside_file.clone();
                    let done = done.clone();
                    let worker = cx.clone();
                    cx.spawn(async move {
                        worker.send_notification(SessionNotification::new(
                            "s",
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new("still streaming")),
                            )),
                        ))?;
                        let tools = worker
                            .send_request(call(id.clone(), "tools", "tools/list", json!({})))
                            .block_task()
                            .await?;
                        let MessageMcpResponse::Result { result, .. } = tools else {
                            panic!("expected tools/list result")
                        };
                        assert_eq!(result["tools"][0]["name"], "tern_open");
                        let response = worker
                            .send_request(call(
                                id,
                                "outside",
                                "tools/call",
                                json!({"name":"tern_open","arguments":{"path":outside}}),
                            ))
                            .block_task()
                            .await?;
                        let MessageMcpResponse::Result { result, .. } = response else {
                            panic!("expected tool result")
                        };
                        assert_eq!(result["isError"], true);
                        done.send(()).unwrap();
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
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
    until(&rx, |e| matches!(e, BridgeEvent::SessionStarted { .. })).await;
    cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
        "open",
    ))]))
    .unwrap();
    tokio::time::timeout(Duration::from_secs(8), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let updates = until(&rx, |e| matches!(e, BridgeEvent::TurnEnded(_))).await;
    assert!(updates.iter().any(|e| matches!(e, BridgeEvent::Update(update) if matches!(&**update,
        SessionUpdate::AgentMessageChunk(chunk) if matches!(&chunk.content, ContentBlock::Text(text) if text.text == "still streaming")))));
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
