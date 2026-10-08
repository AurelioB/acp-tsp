use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use acp_tsp::{
    acp,
    bus::{EventRx, event_channel, wake_pair},
    event::{BridgeEvent, UiCmd},
    launch::SessionOptions,
    model::{EntryKind, Model},
    ui::{composer::Composer, view::view},
};
use agent_client_protocol::schema::v1::{
    ContentBlock, RequestPermissionRequest, RequestPermissionResponse, SessionUpdate, StopReason,
    ToolKind,
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, UntypedMessage};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::Value;
use tern_sdk::{
    Capabilities,
    reconcile::{Doc, Tree},
};
use tokio::sync::mpsc;

const CWD_MARKER: &str = "/tmp/acp-stage3-fixture";

#[derive(Clone, Debug, Deserialize)]
struct Record {
    dir: String,
    line: String,
}

struct Replay {
    lines: Vec<(String, Value)>,
    next: usize,
}

#[derive(Clone)]
struct Message {
    method: String,
    params: Value,
    request: bool,
}

impl Replay {
    fn new(fixture: &str, cwd: &str) -> Self {
        let lines = fixture
            .lines()
            .map(|line| {
                let row: Record = serde_json::from_str(line).unwrap();
                let json = if row.dir == "stderr" {
                    Value::Null
                } else {
                    serde_json::from_str::<Value>(&row.line.replace(CWD_MARKER, cwd)).unwrap()
                };
                (row.dir, json)
            })
            .collect();
        Self { lines, next: 0 }
    }

    fn requests(&self) -> Vec<(String, Value)> {
        self.lines
            .iter()
            .filter(|(dir, value)| {
                dir == "out"
                    && matches!(
                        value.get("method").and_then(Value::as_str),
                        Some("session/prompt" | "session/list")
                    )
            })
            .map(|(_, value)| {
                (
                    value["method"].as_str().unwrap().to_owned(),
                    value["params"].clone(),
                )
            })
            .collect()
    }

    fn permission_outcome(
        &self,
    ) -> Option<agent_client_protocol::schema::v1::RequestPermissionOutcome> {
        let request = self.lines.iter().find_map(|(dir, value)| {
            (dir == "in" && value["method"] == "session/request_permission")
                .then(|| value["id"].clone())
        })?;
        let response = self.lines.iter().find_map(|(dir, value)| {
            (dir == "out" && value.get("id") == Some(&request) && value.get("result").is_some())
                .then(|| value["result"].clone())
        })?;
        let parsed: RequestPermissionResponse = serde_json::from_value(response).unwrap();
        Some(parsed.outcome)
    }

    fn next_request(&mut self, method: &str) -> (Vec<Message>, Value, Vec<Message>) {
        let start = (self.next..self.lines.len())
            .find(|&i| {
                self.lines[i].0 == "out"
                    && self.lines[i].1["method"] == method
                    && self.lines[i].1.get("id").is_some()
            })
            .unwrap_or_else(|| panic!("no recorded client request {method}"));
        let id = self.lines[start].1["id"].clone();
        let response = (start + 1..self.lines.len())
            .find(|&i| {
                self.lines[i].0 == "in"
                    && self.lines[i].1.get("id") == Some(&id)
                    && self.lines[i].1.get("method").is_none()
            })
            .unwrap_or_else(|| panic!("no recorded agent response for {method}"));
        let end = (response + 1..self.lines.len())
            .find(|&i| {
                let (dir, value) = &self.lines[i];
                dir == "out"
                    && value
                        .get("method")
                        .and_then(Value::as_str)
                        .is_some_and(|name| {
                            matches!(
                                name,
                                "initialize" | "session/new" | "session/list" | "session/prompt"
                            )
                        })
            })
            .unwrap_or(self.lines.len());
        let messages = |from: usize, to: usize| {
            (from..to)
                .filter(|&i| {
                    let (dir, value) = &self.lines[i];
                    dir == "in" && value.get("method").and_then(Value::as_str).is_some()
                })
                .map(|i| {
                    let (_, value) = &self.lines[i];
                    Message {
                        method: value["method"].as_str().unwrap().to_owned(),
                        params: value["params"].clone(),
                        request: value.get("id").is_some(),
                    }
                })
                .collect()
        };
        let before = messages(start + 1, response);
        let after = messages(response + 1, end);
        self.next = end;
        let response = &self.lines[response].1;
        assert!(
            response.get("error").is_none(),
            "fixture request {method} failed"
        );
        (before, response["result"].clone(), after)
    }
}

async fn emit(
    messages: Vec<Message>,
    cx: &ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    for message in messages {
        if message.request {
            assert_eq!(
                message.method, "session/request_permission",
                "unexpected agent request"
            );
            let request: RequestPermissionRequest = serde_json::from_value(message.params)?;
            cx.send_request(request).block_task().await?;
        } else {
            cx.send_notification(UntypedMessage::new(&message.method, message.params)?)?;
        }
    }
    Ok(())
}

async fn until(rx: &EventRx, condition: impl Fn(&BridgeEvent) -> bool) -> Vec<BridgeEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        if let Some(batch) = rx.drain() {
            for event in batch {
                let found = condition(&event);
                seen.push(event);
                if found {
                    return seen;
                }
            }
            if seen
                .iter()
                .any(|event| matches!(event, BridgeEvent::Error(_)))
            {
                panic!("replay bridge error: {seen:?}");
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(12),
            "fixture stalled: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[derive(Default)]
struct Observed {
    terminal_output: bool,
    plan_review: bool,
}

fn apply(model: &mut Model, events: Vec<BridgeEvent>, seen: &mut Observed) {
    for event in events {
        assert!(
            !matches!(
                &event,
                BridgeEvent::Error(_) | BridgeEvent::AgentExited { .. }
            ),
            "unexpected replay event: {event:?}"
        );
        if let BridgeEvent::Update(update) = &event
            && let SessionUpdate::ToolCallUpdate(update) = &**update
            && update
                .meta
                .as_ref()
                .is_some_and(|meta| meta.contains_key("terminal_output"))
        {
            seen.terminal_output = true;
        }
        if let BridgeEvent::Permission { request, .. } = &event
            && request.tool_call.fields.kind == Some(ToolKind::SwitchMode)
        {
            seen.plan_review = true;
        }
        model.apply(event);
        let document =
            Doc::from_view(view(model, &Composer::default(), &Capabilities::default())).unwrap();
        let mut ids = HashSet::new();
        fn visit(tree: &Tree, ids: &mut HashSet<String>) {
            assert!(
                ids.insert(tree.id().to_owned()),
                "duplicate rendered id {}",
                tree.id()
            );
            for child in tree.children() {
                visit(child, ids);
            }
        }
        for region in document.regions().iter().flatten() {
            visit(region, &mut ids);
        }
    }
}

async fn replay(
    fixture: &str,
    expect_kind: Option<ToolKind>,
    expect_terminal: bool,
    expect_list: bool,
    expect_review: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().to_str().unwrap();
    let script = Replay::new(fixture, cwd);
    let commands = script.requests();
    let permission = script.permission_outcome();
    let script = Arc::new(Mutex::new(script));
    let (client, server) = Channel::duplex();
    let fake = Agent
        .builder()
        .on_receive_request(
            {
                let script = script.clone();
                async move |request: UntypedMessage, responder, cx: ConnectionTo<Client>| {
                    let (before, result, after) = script.lock().next_request(&request.method);
                    let worker = cx.clone();
                    cx.spawn(async move {
                        emit(before, &worker).await?;
                        responder.respond(result)?;
                        emit(after, &worker).await
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(server);
    let (wake, _) = wake_pair().unwrap();
    let (tx, rx) = event_channel(wake);
    let (cmd, cmds) = mpsc::unbounded_channel();
    let bridge = tokio::spawn(acp::run_with(
        client,
        SessionOptions::new(root.path().into()),
        tx,
        cmds,
    ));
    let agent = tokio::spawn(fake);
    let mut model = Model::default();
    let mut seen = Observed::default();
    apply(
        &mut model,
        until(&rx, |event| {
            matches!(event, BridgeEvent::SessionStarted { .. })
        })
        .await,
        &mut seen,
    );
    let mut ended = 0;
    let mut listed = false;
    for (method, params) in commands {
        match method.as_str() {
            "session/prompt" => {
                let blocks: Vec<ContentBlock> =
                    serde_json::from_value(params["prompt"].clone()).unwrap();
                cmd.send(UiCmd::Prompt(blocks)).unwrap();
                ended += 1;
                loop {
                    let batch = until(&rx, |event| {
                        matches!(
                            event,
                            BridgeEvent::TurnEnded(_) | BridgeEvent::Permission { .. }
                        )
                    })
                    .await;
                    let mut turn_finished = false;
                    for event in &batch {
                        if let BridgeEvent::TurnEnded(Ok(response)) = event {
                            assert_eq!(response.stop_reason, StopReason::EndTurn);
                            turn_finished = true;
                        }
                        if let BridgeEvent::Permission { id, .. } = event {
                            cmd.send(UiCmd::Permission {
                                id: *id,
                                outcome: permission.clone().expect("fixture permission outcome"),
                            })
                            .unwrap();
                        }
                    }
                    apply(&mut model, batch, &mut seen);
                    if turn_finished {
                        break;
                    }
                }
            }
            "session/list" => {
                cmd.send(UiCmd::ListSessions { cursor: None }).unwrap();
                let batch = until(&rx, |event| {
                    matches!(event, BridgeEvent::SessionList { .. })
                })
                .await;
                listed = batch.iter().any(|event| matches!(event, BridgeEvent::SessionList { sessions, .. } if !sessions.is_empty()));
                apply(&mut model, batch, &mut seen);
            }
            _ => unreachable!(),
        }
    }
    assert!(ended > 0);
    assert_eq!(listed, expect_list);
    if let Some(expect_kind) = expect_kind {
        assert!(model.transcript.iter().any(|entry| matches!(&entry.kind, EntryKind::Tool {call, ..} if call.kind == expect_kind)), "expected tool kind {expect_kind:?}");
    }
    assert_eq!(
        seen.terminal_output, expect_terminal,
        "Codex terminal output update did not reach the model"
    );
    assert_eq!(
        seen.plan_review, expect_review,
        "Codex plan review permission was not presented"
    );
    cmd.send(UiCmd::Shutdown).unwrap();
    bridge.await.unwrap().unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn claude_read_tool_and_session_list_replay() {
    replay(
        include_str!("fixtures/claude/read_and_list.jsonl"),
        Some(ToolKind::Read),
        true,
        true,
        false,
    )
    .await;
}

#[tokio::test]
async fn codex_command_terminal_meta_replay() {
    replay(
        include_str!("fixtures/codex/command.jsonl"),
        Some(ToolKind::Execute),
        true,
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn codex_read_only_plan_review_replay() {
    replay(
        include_str!("fixtures/codex/read_only_plan.jsonl"),
        None,
        false,
        false,
        true,
    )
    .await;
}
