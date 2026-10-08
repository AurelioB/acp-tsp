use acp_tsp::{
    event::BridgeEvent,
    model::Model,
    ui::{composer::Composer, view::view},
};
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, PermissionOption, PermissionOptionKind, Plan, PlanEntry,
    PlanEntryPriority, PlanEntryStatus, RequestPermissionRequest, SessionUpdate, TextContent,
    ToolCall, ToolCallContent, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use serde_json::{Value, json};
use tern_sdk::{Capabilities, reconcile::Doc};

fn wire(model: &Model) -> Value {
    let doc = Doc::from_view(view(model, &Composer::default(), &Capabilities::default())).unwrap();
    let mut regions = serde_json::Map::new();
    for (name, tree) in ["main", "dock", "layer"].into_iter().zip(doc.regions()) {
        if let Some(tree) = tree {
            let mut value = serde_json::to_value(tree.wire()).unwrap();
            // age depends on scheduling, not the shape of the view.
            fn normalize(node: &mut Value) {
                if let Some(age) = node.pointer_mut("/p/age") {
                    *age = json!(0);
                }
                if node.pointer("/p/role").and_then(Value::as_str) == Some("omp.user.time") {
                    node["p"]["text"] = json!("<time>");
                    node["p"]["spans"] = json!([{"t":"<time>","s":"dim mono"}]);
                    node["p"]["title"] = json!("<timestamp>");
                }
                if let Some(children) = node.get_mut("c").and_then(Value::as_array_mut) {
                    for child in children {
                        normalize(child);
                    }
                }
            }
            normalize(&mut value);
            regions.insert(name.into(), value);
        }
    }
    Value::Object(regions)
}

fn update(model: &mut Model, update: SessionUpdate) {
    model.apply(BridgeEvent::Update(Box::new(update)));
}

#[test]
fn idle_session_snapshot() {
    insta::assert_json_snapshot!("ui_idle", wire(&Model::default()));
}

#[test]
fn streaming_answer_snapshot() {
    let mut model = Model::default();
    model.submitted("Fix it".into());
    update(
        &mut model,
        SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "Investigating",
        )))),
    );
    update(
        &mut model,
        SessionUpdate::ToolCall(
            ToolCall::new("tool", "Edit source")
                .kind(ToolKind::Edit)
                .name("vendor-specific-write")
                .content(vec![ToolCallContent::from(
                    agent_client_protocol::schema::v1::Diff::new("/tmp/main.rs", "new")
                        .old_text("old"),
                )]),
        ),
    );
    update(
        &mut model,
        SessionUpdate::Plan(Plan::new(vec![PlanEntry::new(
            "Review",
            PlanEntryPriority::High,
            PlanEntryStatus::InProgress,
        )])),
    );
    update(
        &mut model,
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "Fixed.",
        )))),
    );
    insta::assert_json_snapshot!("ui_streaming", wire(&model));
}

#[test]
fn permission_snapshot() {
    let mut model = Model::default();
    model.apply(BridgeEvent::Permission {
        id: 7,
        request: Box::new(RequestPermissionRequest::new(
            "session",
            ToolCallUpdate::new("tool", ToolCallUpdateFields::new().title("Run tests")),
            vec![
                PermissionOption::new("allow", "Allow once", PermissionOptionKind::AllowOnce),
                PermissionOption::new("reject", "Reject once", PermissionOptionKind::RejectOnce),
            ],
        )),
    });
    insta::assert_json_snapshot!("ui_permission", wire(&model));
}

#[test]
fn exited_snapshot() {
    let mut model = Model::default();
    model.apply(BridgeEvent::AgentExited {
        reason: "Process crashed".into(),
    });
    insta::assert_json_snapshot!("ui_exited", wire(&model));
}

#[test]
fn prior_turn_thoughts_stay_collapsed_while_new_turn_streams() {
    let mut model = Model::default();
    model.submitted("first".into());
    update(
        &mut model,
        SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "old thought",
        )))),
    );
    update(
        &mut model,
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "old answer",
        )))),
    );
    model.apply(BridgeEvent::TurnEnded(Ok(Box::new(
        agent_client_protocol::schema::v1::PromptResponse::new(
            agent_client_protocol::schema::v1::StopReason::EndTurn,
        ),
    ))));
    model.submitted("second".into());
    let document = wire(&model);
    let entries = document["main"]["c"].as_array().unwrap();
    let thought = entries
        .iter()
        .find(|entry| entry["p"]["role"] == "omp.thinking")
        .unwrap();
    let assistant = entries
        .iter()
        .find(|entry| entry["p"]["role"] == "omp.assistant")
        .unwrap();
    assert_eq!(thought["p"]["collapsed"], true);
    assert_eq!(assistant["c"][0]["p"]["stream"], false);
}

#[test]
fn many_entries_have_unique_wire_ids() {
    let mut model = Model::default();
    for i in 0..100 {
        model.submitted(format!("prompt {i}"));
        model.apply(BridgeEvent::TurnEnded(Ok(Box::new(
            agent_client_protocol::schema::v1::PromptResponse::new(
                agent_client_protocol::schema::v1::StopReason::EndTurn,
            ),
        ))));
        update(
            &mut model,
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new(format!("answer {i}")),
            ))),
        );
    }
    let doc = Doc::from_view(view(&model, &Composer::default(), &Capabilities::default())).unwrap();
    let mut ids = std::collections::HashSet::new();
    fn visit(tree: &tern_sdk::reconcile::Tree, ids: &mut std::collections::HashSet<String>) {
        assert!(ids.insert(tree.id().to_owned()), "duplicate {}", tree.id());
        for child in tree.children() {
            visit(child, ids);
        }
    }
    for tree in doc.regions().iter().flatten() {
        visit(tree, &mut ids);
    }
}

#[test]
fn auth_picker_is_a_child_of_the_layer_region() {
    // A region root draws no element in Tern: a picker given as the root is invisible.
    let mut model = Model::default();
    model.apply(BridgeEvent::AuthRequired {
        methods: vec![agent_client_protocol::schema::v1::AuthMethod::Agent(
            agent_client_protocol::schema::v1::AuthMethodAgent::new("login", "Log in"),
        )],
        message: None,
    });
    let layer = &wire(&model)["layer"];
    assert_eq!(layer["k"], "col");
    assert_eq!(layer["c"][0]["k"], "picker");
}

#[test]
fn shell_and_read_use_their_omp_tool_frames() {
    let mut model = Model::default();
    model.submitted("Inspect".into());
    update(
        &mut model,
        SessionUpdate::ToolCall(
            ToolCall::new("read", "Read hello.txt")
                .kind(ToolKind::Read)
                .raw_input(json!({"file_path":"hello.txt"}))
                .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed),
        ),
    );
    update(
        &mut model,
        SessionUpdate::ToolCall(
            ToolCall::new("bash", "Run ls")
                .kind(ToolKind::Execute)
                .raw_input(json!({"command":"ls"}))
                .content(vec![ToolCallContent::from(ContentBlock::Text(
                    TextContent::new("hello.txt\n"),
                ))])
                .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed),
        ),
    );
    let doc = wire(&model);
    let entries = doc["main"]["c"].as_array().unwrap();
    let read = entries
        .iter()
        .find(|node| node["p"]["role"] == "omp.tool.read")
        .unwrap();
    assert_eq!(read["p"]["targetKind"], "path");
    assert_eq!(read["p"]["frame"], "inline");
    assert_eq!(read["p"]["status"], "done");
    let bash = entries
        .iter()
        .find(|node| node["p"]["role"] == "omp.tool.bash")
        .unwrap();
    assert_eq!(bash["p"]["target"], "ls");
    assert_eq!(bash["p"]["targetKind"], "command");
    assert_eq!(bash["p"]["frame"], "card");
    assert_eq!(bash["p"]["preview"]["tail"], 10);
    assert_eq!(bash["c"][0]["p"]["role"], "omp.tool.bash.output");
}

#[cfg(feature = "unstable_end_turn_token_usage")]
#[test]
fn completed_turn_usage_is_part_of_the_last_assistant_message() {
    use agent_client_protocol::schema::v1::{Cost, PromptResponse, StopReason, Usage, UsageUpdate};
    let mut model = Model::default();
    update(
        &mut model,
        SessionUpdate::UsageUpdate(UsageUpdate::new(100, 1000).cost(Cost::new(1.0, "USD"))),
    );
    model.submitted("Hi".into());
    update(
        &mut model,
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "Hello.",
        )))),
    );
    update(
        &mut model,
        SessionUpdate::UsageUpdate(UsageUpdate::new(120, 1000).cost(Cost::new(1.02, "USD"))),
    );
    model.apply(BridgeEvent::TurnEnded(Ok(Box::new(
        PromptResponse::new(StopReason::EndTurn).usage(Usage::new(24, 19, 5)),
    ))));
    let doc = wire(&model);
    let entries = doc["main"]["c"].as_array().unwrap();
    let assistant = entries
        .iter()
        .find(|node| node["p"]["role"] == "omp.assistant")
        .unwrap();
    let footer = assistant["c"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["p"]["role"] == "omp.turn.usage")
        .unwrap();
    assert!(
        footer["c"][1]["p"]["text"]
            .as_str()
            .unwrap()
            .contains("19 in · 5 out · $0.02")
    );
    assert!(!doc["main"].to_string().contains("Tokens:"));
}

#[test]
fn codex_exec_command_renders_as_a_shell_card_with_its_output() {
    // Codex labels every shell command by intent (`kind: read`, "List files") with no path.
    let mut model = Model::default();
    let mut call = ToolCall::new("exec-1", "List files")
        .kind(ToolKind::Read)
        .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed)
        .content(vec![ToolCallContent::from("a\nb\n")]);
    call.name = Some("exec_command".into());
    update(&mut model, SessionUpdate::ToolCall(call));
    let doc = wire(&model);
    let tool = doc["main"]["c"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["k"] == "tool")
        .unwrap();
    assert_eq!(tool["p"]["role"], "omp.tool.bash");
    assert_eq!(tool["p"]["target"], "List files");
    assert_eq!(tool["p"]["targetKind"], "text");
    assert_eq!(tool["c"][0]["k"], "ansi");
    assert_eq!(tool["c"][0]["p"]["text"], "a\nb\n");
}

#[test]
fn shell_targets_keep_command_styling_and_output_survives_updates() {
    let mut model = Model::default();
    // A real command argument stays a command target.
    update(
        &mut model,
        SessionUpdate::ToolCall(
            ToolCall::new("bash-1", "Run tests")
                .kind(ToolKind::Execute)
                .raw_input(serde_json::json!({"command": "cargo test"})),
        ),
    );
    // A Codex exec_command: output arrives in a later update, then a status-only update.
    let mut call = ToolCall::new("exec-1", "List files").kind(ToolKind::Read);
    call.name = Some("exec_command".into());
    update(&mut model, SessionUpdate::ToolCall(call));
    update(
        &mut model,
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "exec-1",
            ToolCallUpdateFields::new().content(vec![ToolCallContent::from("a\nb\n")]),
        )),
    );
    update(
        &mut model,
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "exec-1",
            ToolCallUpdateFields::new()
                .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed),
        )),
    );
    let doc = wire(&model);
    let tools: Vec<_> = doc["main"]["c"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["k"] == "tool")
        .collect();
    assert_eq!(tools[0]["p"]["target"], "cargo test");
    assert_eq!(tools[0]["p"]["targetKind"], "command");
    assert_eq!(tools[1]["p"]["role"], "omp.tool.bash");
    assert_eq!(tools[1]["p"]["status"], "done");
    assert_eq!(tools[1]["c"][0]["p"]["text"], "a\nb\n");
}
