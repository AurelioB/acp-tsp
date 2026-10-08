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
fn many_entries_have_unique_wire_ids() {
    let mut model = Model::default();
    for i in 0..100 {
        model.submitted(format!("prompt {i}"));
        model.apply(BridgeEvent::TurnEnded(Ok(
            agent_client_protocol::schema::v1::StopReason::EndTurn,
        )));
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
