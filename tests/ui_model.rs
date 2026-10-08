use acp_tsp::{
    event::BridgeEvent,
    model::{EntryKind, Model, Turn},
};
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, PermissionOption, PermissionOptionKind, Plan, PlanEntry,
    PlanEntryPriority, PlanEntryStatus, RequestPermissionRequest, SessionUpdate, StopReason,
    TextContent, ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};

fn update(model: &mut Model, value: SessionUpdate) {
    model.apply(BridgeEvent::Update(Box::new(value)));
}

#[test]
fn chunks_group_by_adjacency_and_message_id() {
    let mut model = Model::default();
    for (id, text, thought) in [
        ("one", "Hello", false),
        ("one", " world", false),
        ("one", "thinking", true),
        ("one", "!", false),
        ("two", "next", false),
    ] {
        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text))).message_id(id);
        update(
            &mut model,
            if thought {
                SessionUpdate::AgentThoughtChunk(chunk)
            } else {
                SessionUpdate::AgentMessageChunk(chunk)
            },
        );
    }
    assert_eq!(model.transcript.len(), 4);
    assert!(
        matches!(&model.transcript[0].kind, EntryKind::Agent { text, .. } if text == "Hello world")
    );
    assert!(matches!(&model.transcript[2].kind, EntryKind::Agent { text, .. } if text == "!"));
    assert!(
        model
            .transcript
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
    );
}

#[test]
fn resource_links_remain_clickable_markdown_not_debug_text() {
    let mut model = Model::default();
    update(
        &mut model,
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::ResourceLink(
            agent_client_protocol::schema::v1::ResourceLink::new(
                "file]name",
                "file:///tmp/(file).rs",
            ),
        ))),
    );
    assert!(
        matches!(&model.transcript[0].kind, EntryKind::Agent { text, .. }
        if text == "[file\\]name](file:///tmp/\\(file\\).rs)")
    );
}

#[test]
fn local_prompt_suppresses_only_its_matching_echo_even_when_queued() {
    let mut model = Model::default();
    model.submitted("first".into());
    model.submitted("second".into());
    assert_eq!(model.queued, 1);
    let echo = |text: &str| {
        SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text,
        ))))
    };
    update(&mut model, echo("fir"));
    update(&mut model, echo("st"));
    assert_eq!(model.transcript.len(), 2);
    model.apply(BridgeEvent::TurnEnded(Ok(StopReason::EndTurn)));
    assert_eq!(model.queued, 0);
    update(&mut model, echo("second"));
    assert_eq!(model.transcript.len(), 2);
    update(&mut model, echo("a different user message"));
    assert!(
        matches!(&model.transcript[2].kind, EntryKind::User { text, local: false, .. } if text == "a different user message")
    );
}

#[test]
fn tool_updates_merge_only_present_fields_and_unknown_id_is_visible() {
    let mut model = Model::default();
    let mut call = ToolCall::new("known", "Original");
    call.status = ToolCallStatus::InProgress;
    call.content = vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(
        "first",
    )))];
    update(&mut model, SessionUpdate::ToolCall(call));
    update(
        &mut model,
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "known",
            ToolCallUpdateFields::new()
                .title("Renamed")
                .raw_output(serde_json::json!({"ok":true})),
        )),
    );
    update(
        &mut model,
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "unknown",
            ToolCallUpdateFields::new()
                .title("Late")
                .status(ToolCallStatus::Completed),
        )),
    );
    let EntryKind::Tool { call, .. } = &model.transcript[0].kind else {
        panic!("tool")
    };
    assert_eq!(call.title, "Renamed");
    assert_eq!(call.status, ToolCallStatus::InProgress);
    assert_eq!(call.content.len(), 1);
    assert_eq!(call.raw_output, Some(serde_json::json!({"ok":true})));
    let EntryKind::Tool { call, .. } = &model.transcript[1].kind else {
        panic!("tool")
    };
    assert_eq!(
        (call.title.as_str(), call.status),
        ("Late", ToolCallStatus::Completed)
    );
}

#[test]
fn plan_replaces_and_cancel_marks_only_unfinished_tools() {
    let mut model = Model::default();
    model.submitted("go".into());
    update(
        &mut model,
        SessionUpdate::Plan(Plan::new(vec![PlanEntry::new(
            "first",
            PlanEntryPriority::High,
            PlanEntryStatus::Pending,
        )])),
    );
    update(
        &mut model,
        SessionUpdate::ToolCall(ToolCall::new("running", "Run").status(ToolCallStatus::InProgress)),
    );
    update(
        &mut model,
        SessionUpdate::ToolCall(
            ToolCall::new("finished", "Done").status(ToolCallStatus::Completed),
        ),
    );
    update(
        &mut model,
        SessionUpdate::Plan(Plan::new(vec![PlanEntry::new(
            "second",
            PlanEntryPriority::Low,
            PlanEntryStatus::Completed,
        )])),
    );
    assert_eq!(
        model
            .transcript
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Plan(_)))
            .count(),
        1
    );
    model.apply(BridgeEvent::TurnEnded(Ok(StopReason::Cancelled)));
    assert!(matches!(model.turn, Turn::Idle));
    assert!(matches!(
        &model.transcript[2].kind,
        EntryKind::Tool {
            cancelled: true,
            ..
        }
    ));
    assert!(matches!(
        &model.transcript[3].kind,
        EntryKind::Tool {
            cancelled: false,
            ..
        }
    ));
}

#[test]
fn cancelling_queued_turn_does_not_relabel_prior_tools() {
    let mut model = Model::default();
    model.submitted("first".into());
    model.submitted("queued".into());
    update(
        &mut model,
        SessionUpdate::ToolCall(
            ToolCall::new("first-tool", "First").status(ToolCallStatus::InProgress),
        ),
    );
    model.apply(BridgeEvent::TurnEnded(Ok(StopReason::EndTurn)));
    update(
        &mut model,
        SessionUpdate::ToolCall(
            ToolCall::new("second-tool", "Second").status(ToolCallStatus::InProgress),
        ),
    );
    model.apply(BridgeEvent::TurnEnded(Ok(StopReason::Cancelled)));
    assert!(matches!(
        &model.transcript[2].kind,
        EntryKind::Tool {
            cancelled: false,
            ..
        }
    ));
    assert!(matches!(
        &model.transcript[3].kind,
        EntryKind::Tool {
            cancelled: true,
            ..
        }
    ));
}

#[test]
fn permissions_resolve_out_of_order_without_reordering_dialogs() {
    let mut model = Model::default();
    let request = || {
        Box::new(RequestPermissionRequest::new(
            "session",
            ToolCallUpdate::new("tool", ToolCallUpdateFields::new().title("Run")),
            vec![PermissionOption::new(
                "deny",
                "Deny",
                PermissionOptionKind::RejectOnce,
            )],
        ))
    };
    for id in [4, 9, 2] {
        model.apply(BridgeEvent::Permission {
            id,
            request: request(),
        });
    }
    assert_eq!(model.oldest_permission().unwrap().id, 4);
    model.apply(BridgeEvent::Resolved(9));
    assert_eq!(
        model.permissions.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![4, 2]
    );
    model.apply(BridgeEvent::Resolved(4));
    assert_eq!(model.oldest_permission().unwrap().id, 2);
}
