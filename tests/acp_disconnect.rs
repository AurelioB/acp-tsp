use std::time::{Duration, Instant};

use acp_tsp::acp;
use acp_tsp::bus::{event_channel, wake_pair};
use acp_tsp::event::{BridgeEvent, UiCmd};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PermissionOption, PermissionOptionKind, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, TextContent, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo};
use tokio::sync::mpsc;

#[tokio::test]
async fn agent_exit_dismisses_pending_permission() {
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
            async |_: NewSessionRequest, responder, _cx| {
                responder.respond(NewSessionResponse::new("s"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: PromptRequest, _responder, cx: ConnectionTo<Client>| {
                cx.send_request(RequestPermissionRequest::new(
                    request.session_id,
                    ToolCallUpdate::new("tool", ToolCallUpdateFields::new()),
                    vec![PermissionOption::new(
                        "yes",
                        "Allow",
                        PermissionOptionKind::AllowOnce,
                    )],
                ))
                .detach();
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
    let start = Instant::now();
    let mut permission = None;
    while permission.is_none() {
        for event in rx.drain().unwrap_or_default() {
            if let BridgeEvent::SessionStarted { .. } = event {
                cmd.send(UiCmd::Prompt(vec![ContentBlock::Text(TextContent::new(
                    "prompt",
                ))]))
                .unwrap();
            }
            if let BridgeEvent::Permission { id, .. } = event {
                permission = Some(id);
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "permission did not arrive"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let id = permission.unwrap();
    agent.abort();
    let start = Instant::now();
    let mut exited = false;
    let mut resolved = false;
    while !exited || !resolved {
        for event in rx.drain().unwrap_or_default() {
            if matches!(event, BridgeEvent::AgentExited { .. }) {
                exited = true;
            }
            if matches!(event, BridgeEvent::Resolved(actual) if actual == id) {
                resolved = true;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "exit={exited}, resolved={resolved}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    cmd.send(UiCmd::Permission {
        id,
        outcome: RequestPermissionOutcome::Cancelled,
    })
    .unwrap();
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
}
