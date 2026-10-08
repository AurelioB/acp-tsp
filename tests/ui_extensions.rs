use acp_tsp::{
    event::BridgeEvent,
    model::{EntryKind, Model},
    ui::{composer::Composer, view::view},
};
use agent_client_protocol::schema::v1::RequestPermissionRequest;
use serde_json::{Value, json};
use tern_sdk::{Capabilities, reconcile::Doc};

fn wire(model: &Model) -> Value {
    let doc = Doc::from_view(view(model, &Composer::default(), &Capabilities::default())).unwrap();
    let mut regions = serde_json::Map::new();
    for (name, tree) in ["main", "dock", "layer"].into_iter().zip(doc.regions()) {
        if let Some(tree) = tree {
            regions.insert(name.into(), serde_json::to_value(tree.wire()).unwrap());
        }
    }
    Value::Object(regions)
}

fn update(model: &mut Model, value: Value) {
    model.apply(BridgeEvent::Update(Box::new(
        serde_json::from_value(value).unwrap(),
    )));
}

#[test]
fn terminal_deltas_and_exit_are_bounded_and_visible() {
    let mut model = Model::default();
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call", "toolCallId":"shell", "title":"Run", "kind":"execute", "_meta": {"terminal_info":{"terminal_id":"shell","cwd":"/tmp"},"terminal_output_delta":{"terminal_id":"shell","data":"first"}}}),
    );
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"shell", "_meta": {"terminal_output_delta":{"terminal_id":"shell","data":" second"}, "terminal_exit":{"terminal_id":"shell","exit_code":3}}}),
    );
    let state = &model.ext.tools[&"shell".into()];
    assert_eq!(state.output, "first second");
    assert_eq!(state.exit_code, Some(3));
    insta::assert_json_snapshot!("ui_extension_terminal", wire(&model)["main"]);
    let huge = "x".repeat(300_000);
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"shell", "_meta": {"terminal_output_delta":{"data":huge}}}),
    );
    assert!(model.ext.tools[&"shell".into()].truncated);
    assert_eq!(model.ext.tools[&"shell".into()].output.len(), 256 * 1024);
}

#[test]
fn zed_output_chunks_append_but_full_snapshots_replace() {
    let mut model = Model::default();
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call", "toolCallId":"cmd", "title":"Run", "_meta":{"terminal_info":{"terminal_id":"cmd"}, "terminal_output":{"terminal_id":"cmd","data":"first"}}}),
    );
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"cmd", "_meta":{"terminal_output":{"data":" second"}}}),
    );
    assert_eq!(model.ext.tools[&"cmd".into()].output, "first second");
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"cmd", "_meta":{"terminal_output":{"data":"first second third"}}}),
    );
    assert_eq!(model.ext.tools[&"cmd".into()].output, "first second third");
}

#[test]
fn child_tools_nest_inside_the_parent_even_when_child_arrives_first() {
    let mut model = Model::default();
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call", "toolCallId":"child", "title":"Read child file", "_meta":{"claudeCode":{"toolName":"Read","parentToolUseId":"parent"}}}),
    );
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call", "toolCallId":"parent", "title":"Investigating", "_meta":{"claudeCode":{"toolName":"Agent"}}}),
    );
    assert_eq!(
        model.ext.tools[&"child".into()].parent.as_deref(),
        Some("parent")
    );
    insta::assert_json_snapshot!("ui_extension_subagent", wire(&model)["main"]);
}

#[test]
fn claude_write_without_acp_diff_still_displays_written_source() {
    let mut model = Model::default();
    update(
        &mut model,
        json!({
            "sessionUpdate":"tool_call", "toolCallId":"write", "title":"Write file",
            "kind":"edit", "rawInput":{"file_path":"note.txt","content":"hello\n"},
            "_meta":{"claudeCode":{"toolName":"Write"}}
        }),
    );
    let document = wire(&model);
    let tool = document["main"]["c"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["p"]["role"] == "omp.tool.write")
        .unwrap();
    assert_eq!(tool["p"]["target"], "note.txt");
    assert_eq!(tool["c"][0]["p"]["role"], "omp.tool.write.source");
    assert_eq!(tool["c"][0]["p"]["text"], "hello\n");
}

#[test]
fn plan_permission_shows_markdown_above_decisions() {
    let mut model = Model::default();
    let request: RequestPermissionRequest = serde_json::from_value(json!({
        "sessionId":"s", "toolCall":{"toolCallId":"plan-review:123", "kind":"switch_mode", "rawInput":{"plan":"# Keep tests green\n\nDo the thing."}},
        "options":[{"optionId":"implement_plan","name":"Implement","kind":"allow_once"},{"optionId":"revise_plan","name":"Revise","kind":"reject_once"}]
    })).unwrap();
    model.apply(BridgeEvent::Permission {
        id: 5,
        request: Box::new(request),
    });
    let layer = wire(&model)["layer"].clone();
    assert!(layer.to_string().contains("# Keep tests green"));
    insta::assert_json_snapshot!("ui_extension_plan_approval", layer);
}

#[test]
fn account_chip_never_leaks_email_until_details_open() {
    let mut model = Model::default();
    model.apply(BridgeEvent::Extension { method: "_auth/status_update".into(), params: json!({"authStatus":{"kind":"account","label":"ChatGPT Pro","account":{"email":"private@example.org","plan":"pro"}}}) });
    let normal = wire(&model);
    assert!(normal["dock"].to_string().contains("ChatGPT Pro"));
    assert!(!normal.to_string().contains("private@example.org"));
    insta::assert_json_snapshot!("ui_extension_account", normal["dock"]);
    model.ext.account_details = true;
    assert!(
        wire(&model)["layer"]
            .to_string()
            .contains("private@example.org")
    );
}

#[test]
fn metadata_tolerates_missing_optional_fields_and_handles_mcp_progress() {
    let mut model = Model::default();
    update(
        &mut model,
        json!({"sessionUpdate":"tool_call", "toolCallId":"mcp", "title":"MCP", "_meta":{"is_mcp_tool_call":true,"terminal_info":{},"claudeCode":{},"mcp_output_delta":{"data":"progress"}}}),
    );
    let meta = &model.ext.tools[&"mcp".into()];
    assert!(meta.mcp);
    assert_eq!(meta.mcp_output, "progress");
    assert!(meta.parent.is_none());
    assert!(matches!(&model.transcript[0].kind, EntryKind::Tool { .. }));
}

#[test]
fn advertised_goal_and_steering_require_explicit_capabilities() {
    let mut model = Model::default();
    let init = serde_json::from_value(json!({
        "protocolVersion":1,
        "agentCapabilities":{"_meta":{"authStatus":{}, "claudeCode":{"promptQueueing":true}}},
        "_meta":{"steering":{"supported":true},"jetbrains":{"air":{"goal":{"controlMethod":"_session/goal","actions":["set","clear"]}}}}
    })).unwrap();
    model.apply(BridgeEvent::Initialized(Box::new(init)));
    assert!(model.ext.steering);
    assert!(model.ext.auth_advertised);
    assert_eq!(model.ext.goal.as_ref().unwrap().actions, ["set", "clear"]);

    let mut plain = Model::default();
    plain.apply(BridgeEvent::Initialized(Box::new(
        serde_json::from_value(json!({"protocolVersion":1,"_meta":{"steering":{}}})).unwrap(),
    )));
    assert!(!plain.ext.steering);
    assert!(plain.ext.goal.is_none());
}

#[test]
fn claude_plan_permission_and_rate_warning() {
    let mut model = Model::default();
    let request: RequestPermissionRequest = serde_json::from_value(json!({
        "sessionId":"s", "toolCall":{"toolCallId":"exit-tool", "kind":"switch_mode", "rawInput":{"plan":"# Claude plan"}},
        "options":[{"optionId":"exit-plan-default","name":"Approve","kind":"allow_once"}]
    })).unwrap();
    model.apply(BridgeEvent::Permission {
        id: 7,
        request: Box::new(request),
    });
    assert!(wire(&model)["layer"].to_string().contains("# Claude plan"));
    model
        .ext
        .rate(&json!({"status":"rejected","resetsAt":4_000_000_000u64}));
    assert!(
        model
            .ext
            .rate_limit
            .as_deref()
            .unwrap()
            .contains("resets in")
    );
    model.ext.rate(&json!({"status":"allowed"}));
    assert!(model.ext.rate_limit.is_none());
}

#[test]
fn quota_metadata_does_not_duplicate_the_context_meter() {
    let mut model = Model::default();
    update(
        &mut model,
        json!({"sessionUpdate":"usage_update","used":200,"size":1000,
        "_meta":{"quota":{"token_count":{"totalTokens":501,"inputTokens":450},"model_usage":[]}}}),
    );
    assert_eq!(model.ext.quota_tokens, Some(501));
    // The composer meter reports context consumption; quota metadata is not a capacity.
    let dock = wire(&model)["dock"].to_string();
    assert!(dock.contains("Context: 200 of 1000 tokens"));
    assert_eq!(dock.matches("\"k\":\"meter\"").count(), 1);
}
