#![cfg(feature = "unstable_llm_providers")]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
        AgentCapabilities, DisableProviderRequest, DisableProviderResponse, InitializeRequest,
        InitializeResponse, ListProvidersRequest, ListProvidersResponse, LlmProtocol,
        NewSessionRequest, NewSessionResponse, ProviderInfo, ProvidersCapabilities,
        SetProviderRequest, SetProviderResponse,
    },
};
use agent_client_protocol::{Agent, Channel};
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

#[tokio::test]
async fn provider_changes_refresh_list_without_exposing_credentials() {
    let root = tempfile::tempdir().unwrap();
    let (client, server) = Channel::duplex();
    let enabled = Arc::new(AtomicBool::new(false));
    let fake = Agent
        .builder()
        .on_receive_request(
            async |_: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new().providers(ProvidersCapabilities::new()),
                    ),
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
        .on_receive_request(
            {
                let enabled = enabled.clone();
                async move |_: ListProvidersRequest, responder, _cx| {
                    responder.respond(ListProvidersResponse::new(vec![
                        ProviderInfo::new("primary", vec![LlmProtocol::OpenAi], false, None).meta(
                            serde_json::Map::from_iter([(
                                "enabled".into(),
                                enabled.load(Ordering::SeqCst).into(),
                            )]),
                        ),
                    ]))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let enabled = enabled.clone();
                async move |request: SetProviderRequest, responder, _cx| {
                    assert_eq!(request.provider_id.0.as_ref(), "primary");
                    enabled.store(true, Ordering::SeqCst);
                    responder.respond(SetProviderResponse::new())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let enabled = enabled.clone();
                async move |request: DisableProviderRequest, responder, _cx| {
                    assert_eq!(request.provider_id.0.as_ref(), "primary");
                    enabled.store(false, Ordering::SeqCst);
                    responder.respond(DisableProviderResponse::new())
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
    cmd.send(UiCmd::ListProviders).unwrap();
    let first = until(&rx, |e| matches!(e, BridgeEvent::Providers(_))).await;
    assert!(
        matches!(first.last(), Some(BridgeEvent::Providers(list)) if list.providers[0].meta.as_ref().unwrap()["enabled"] == false)
    );
    cmd.send(UiCmd::SetProvider(Box::new(SetProviderRequest::new(
        "primary",
        LlmProtocol::OpenAi,
        "https://api.example.test",
    ))))
    .unwrap();
    let second = until(&rx, |e| matches!(e, BridgeEvent::Providers(_))).await;
    assert!(
        matches!(second.last(), Some(BridgeEvent::Providers(list)) if list.providers[0].meta.as_ref().unwrap()["enabled"] == true)
    );
    cmd.send(UiCmd::DisableProvider(Box::new(
        DisableProviderRequest::new("primary"),
    )))
    .unwrap();
    let third = until(&rx, |e| matches!(e, BridgeEvent::Providers(_))).await;
    assert!(
        matches!(third.last(), Some(BridgeEvent::Providers(list)) if list.providers[0].meta.as_ref().unwrap()["enabled"] == false)
    );
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}
