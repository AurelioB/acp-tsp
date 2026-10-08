use std::fs;

use acp_tsp::{
    event::{BridgeEvent, TerminalEvent},
    model::Model,
    ui::attach,
};
use agent_client_protocol::schema::v1::{
    ContentBlock, EmbeddedResourceResource, PromptCapabilities, TerminalExitStatus,
};
#[cfg(any(feature = "unstable_plan_operations", feature = "unstable_session_compaction"))]
use acp_tsp::model::EntryKind;
#[cfg(any(feature = "unstable_plan_operations", feature = "unstable_session_compaction"))]
use agent_client_protocol::schema::v1::SessionUpdate;

#[test]
fn terminal_output_keeps_last_utf8_boundary_and_exit_snapshot() {
    let mut model = Model::default();
    let id: agent_client_protocol::schema::v1::TerminalId = "terminal".into();
    model.apply(BridgeEvent::Terminal(TerminalEvent::Created {
        id: id.clone(),
        command: "echo hi".into(),
        cwd: "/tmp".into(),
    }));
    model.apply(BridgeEvent::Terminal(TerminalEvent::Output {
        id: id.clone(),
        chunk: format!("{}é", "a".repeat(256 * 1024 - 1)),
        truncated: false,
    }));
    let terminal = &model.content.terminals[&id];
    assert_eq!(terminal.output.len(), 256 * 1024);
    assert!(terminal.output.ends_with('é'));
    assert!(terminal.truncated);
    model.apply(BridgeEvent::Terminal(TerminalEvent::Exited {
        id: id.clone(),
        status: TerminalExitStatus::new().exit_code(7),
    }));
    model.apply(BridgeEvent::Terminal(TerminalEvent::Released(id.clone())));
    let terminal = &model.content.terminals[&id];
    assert_eq!(
        terminal.status.as_ref().and_then(|status| status.exit_code),
        Some(7)
    );
    assert!(terminal.released);
    assert!(terminal.output.ends_with('é'));
}

#[test]
fn attachments_are_capability_gated_and_confined_to_granted_roots() {
    let dir = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("code.rs"), "fn main() {}").unwrap();
    fs::write(dir.path().join("pic.png"), b"\x89PNG").unwrap();
    fs::write(dir.path().join("voice.wav"), b"RIFF").unwrap();
    fs::write(extra.path().join("extra.txt"), "hello").unwrap();
    fs::write(outside.path().join("secret.txt"), "secret").unwrap();
    let roots = vec![extra.path().to_path_buf()];
    let caps = PromptCapabilities::new()
        .image(true)
        .audio(true)
        .embedded_context(true);
    let text = format!(
        "Review @code.rs @pic.png @voice.wav @{}",
        extra.path().join("extra.txt").display()
    );
    let blocks = attach::prompt(&text, dir.path(), &roots, Some(&caps)).unwrap();
    assert!(matches!(&blocks[0], ContentBlock::Text(body) if body.text == text));
    assert_eq!(
        blocks
            .iter()
            .filter(|block| matches!(block, ContentBlock::ResourceLink(_)))
            .count(),
        4
    );
    assert!(blocks.iter().any(
        |block| matches!(block, ContentBlock::Image(image) if image.mime_type == "image/png")
    ));
    assert!(blocks.iter().any(
        |block| matches!(block, ContentBlock::Audio(audio) if audio.mime_type == "audio/wav")
    ));
    assert!(blocks.iter().any(|block| matches!(block, ContentBlock::Resource(resource) if matches!(&resource.resource, EmbeddedResourceResource::TextResourceContents(contents) if contents.text == "hello"))));
    let links_only = attach::prompt("@pic.png @voice.wav @code.rs", dir.path(), &[], None).unwrap();
    assert_eq!(links_only.len(), 4);
    assert!(
        matches!(attach::prompt(&format!("@{}", outside.path().join("secret.txt").display()), dir.path(), &roots, Some(&caps)), Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("linked.txt"),
        )
        .unwrap();
        assert!(attach::prompt("@linked.txt", dir.path(), &roots, None).is_err());
    }
}

#[cfg(feature = "unstable_plan_operations")]
#[test]
fn identified_plans_replace_by_id_and_remove_only_target() {
    use agent_client_protocol::schema::v1::{PlanRemoved, PlanUpdate, PlanUpdateContent};
    let mut model = Model::default();
    for (id, body) in [("alpha", "first"), ("beta", "other"), ("alpha", "replaced")] {
        model.apply(BridgeEvent::Update(Box::new(SessionUpdate::PlanUpdate(
            PlanUpdate::new(PlanUpdateContent::markdown(id, body)),
        ))));
    }
    assert_eq!(model.transcript.len(), 2);
    assert!(
        matches!(&model.transcript[0].kind, EntryKind::IdentifiedPlan(PlanUpdateContent::Markdown(plan)) if plan.content == "replaced")
    );
    model.apply(BridgeEvent::Update(Box::new(SessionUpdate::PlanRemoved(
        PlanRemoved::new("alpha"),
    ))));
    assert!(
        matches!(&model.transcript[..], [entry] if matches!(&entry.kind, EntryKind::IdentifiedPlan(PlanUpdateContent::Markdown(plan)) if plan.plan_id.to_string() == "beta"))
    );
}

#[cfg(feature = "unstable_session_compaction")]
#[test]
fn compaction_patches_summary_until_completed() {
    use agent_client_protocol::schema::v1::{
        CompactionStatus, CompactionSummaryChunk, CompactionUpdate, TextContent,
    };
    let mut model = Model::default();
    let apply = |model: &mut Model, update| model.apply(BridgeEvent::Update(Box::new(update)));
    apply(
        &mut model,
        SessionUpdate::CompactionUpdate(CompactionUpdate::new("ctx", CompactionStatus::InProgress)),
    );
    apply(
        &mut model,
        SessionUpdate::CompactionSummaryChunk(CompactionSummaryChunk::new(
            "ctx",
            ContentBlock::Text(TextContent::new("first")),
        )),
    );
    apply(
        &mut model,
        SessionUpdate::CompactionSummaryChunk(CompactionSummaryChunk::new(
            "ctx",
            ContentBlock::Text(TextContent::new(" second")),
        )),
    );
    apply(
        &mut model,
        SessionUpdate::CompactionUpdate(CompactionUpdate::new("ctx", CompactionStatus::Completed)),
    );
    assert!(
        matches!(&model.transcript[..], [entry] if matches!(&entry.kind, EntryKind::Compaction { status: CompactionStatus::Completed, summary, .. } if matches!(&summary[..], [ContentBlock::Text(text)] if text.text == "first second")))
    );
}

#[cfg(feature = "unstable_subagents")]
#[test]
fn subagent_message_upserts_then_streams_without_losing_metadata() {
    use agent_client_protocol::schema::{MaybeUndefined, v1::{SessionMessage, SessionMessageChunk, SubagentUpdate, TextContent}};
    let mut model = Model::default();
    let update = |model: &mut Model, update| model.apply(BridgeEvent::Update(Box::new(update)));
    update(&mut model, SessionUpdate::SubagentUpdate(SubagentUpdate::new("child").title("Researcher".to_owned())));
    update(&mut model, SessionUpdate::SessionMessage(SessionMessage::new("msg")
        .sender_session_id(Some("child".into())).content(vec![ContentBlock::Text(TextContent::new("Find"))])));
    update(&mut model, SessionUpdate::SessionMessageChunk(SessionMessageChunk::new("msg", ContentBlock::Text(TextContent::new("ings")))));
    let EntryKind::Subagent { title, messages, .. } = &model.transcript[0].kind else { panic!("child") };
    assert_eq!(title, "Researcher");
    assert!(matches!(&messages[0].content, MaybeUndefined::Value(blocks)
        if matches!(&blocks[..], [ContentBlock::Text(text)] if text.text == "Findings")));
}

#[cfg(feature = "unstable_end_turn_token_usage")]
#[test]
fn end_turn_usage_shows_per_turn_deltas_not_cumulative_totals() {
    use agent_client_protocol::schema::v1::{PromptResponse, StopReason, Usage};
    let mut model = Model::default();
    for (total, input, output) in [(17, 11, 6), (29, 20, 9)] {
        model.apply(BridgeEvent::TurnEnded(Ok(Box::new(PromptResponse::new(StopReason::EndTurn)
            .usage(Usage::new(total, input, output))))));
    }
    assert!(matches!(&model.transcript[1].kind, EntryKind::Usage(usage)
        if (usage.total_tokens, usage.input_tokens, usage.output_tokens) == (12, 9, 3)));
}
