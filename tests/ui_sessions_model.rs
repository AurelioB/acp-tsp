use acp_tsp::{
    event::{BridgeEvent, SessionStartKind, UiCmd},
    model::{EntryKind, Model},
    ui::{
        palette::{Choice, Kind, Palette},
        prefs,
    },
};
use agent_client_protocol::schema::v1::{
    AgentCapabilities, AvailableCommand, AvailableCommandsUpdate, ConfigOptionUpdate, ContentBlock,
    ContentChunk, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SessionInfo, SessionInfoUpdate, SessionMode, SessionModeState, SessionUpdate, TextContent,
};
use serde_json::json;

fn start(model: &mut Model) {
    model.apply(BridgeEvent::SessionStarted {
        session_id: "s1".into(),
        kind: SessionStartKind::New,
        cwd: "/tmp/project".into(),
        additional_dirs: vec!["/tmp/shared".into()],
        modes: Some(SessionModeState::new(
            "ask",
            vec![
                SessionMode::new("ask", "Ask"),
                SessionMode::new("code", "Code"),
            ],
        )),
        config_options: None,
    });
}

#[test]
fn switch_resets_old_turn_transcript_and_dialogs_but_load_replays_history() {
    let mut model = Model::default();
    start(&mut model);
    assert_eq!(model.session.as_ref().unwrap().kind, SessionStartKind::New);
    assert_eq!(
        model.session.as_ref().unwrap().additional_dirs,
        vec![std::path::PathBuf::from("/tmp/shared")]
    );
    model.submitted("prior prompt".into());
    model.apply(BridgeEvent::Update(Box::new(SessionUpdate::Plan(
        agent_client_protocol::schema::v1::Plan::new(vec![]),
    ))));
    model.apply(BridgeEvent::AuthRequired {
        methods: vec![],
        message: None,
    });
    model.apply(BridgeEvent::SessionStarting(SessionStartKind::Load));
    assert!(model.session.is_none());
    assert!(model.transcript.is_empty());
    assert!(model.auth.is_none());
    assert!(model.status.contains("Loading"));
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "replayed",
        )))),
    )));
    let title: SessionInfoUpdate =
        serde_json::from_value(json!({"title": "Replayed title"})).unwrap();
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::SessionInfoUpdate(title),
    )));
    model.apply(BridgeEvent::SessionStarted {
        session_id: "s2".into(),
        kind: SessionStartKind::Load,
        cwd: "/tmp/elsewhere".into(),
        additional_dirs: vec![],
        modes: None,
        config_options: None,
    });
    assert_eq!(model.session.as_ref().unwrap().id.0.as_ref(), "s2");
    assert_eq!(model.session.as_ref().unwrap().kind, SessionStartKind::Load);
    assert_eq!(
        model.session.as_ref().unwrap().title.as_deref(),
        Some("Replayed title")
    );
    assert!(
        matches!(&model.transcript[0].kind, EntryKind::Agent { text, .. } if text == "replayed")
    );
    model.apply(BridgeEvent::SessionClosed);
    assert!(model.session.is_none());
}

#[test]
fn logout_then_auth_required_reopens_authentication() {
    use agent_client_protocol::schema::v1::{AuthMethod, AuthMethodAgent};
    let mut model = Model::default();
    start(&mut model);
    model.apply(BridgeEvent::LoggedOut);
    assert!(model.session.is_none());
    model.apply(BridgeEvent::AuthRequired {
        methods: vec![AuthMethod::Agent(AuthMethodAgent::new("login", "Log in"))],
        message: None,
    });
    assert_eq!(model.auth.as_ref().unwrap().methods[0].name(), "Log in");
}

#[test]
fn list_paging_replaces_then_appends_and_delete_removes() {
    let mut model = Model::default();
    let first = SessionInfo::new("a", "/tmp/project");
    model.apply(BridgeEvent::SessionList {
        sessions: vec![first],
        next_cursor: Some("next".into()),
        first_page: true,
    });
    model.apply(BridgeEvent::SessionList {
        sessions: vec![SessionInfo::new("b", "/tmp/project")],
        next_cursor: None,
        first_page: false,
    });
    assert_eq!(model.sessions.items.len(), 2);
    assert_eq!(model.sessions.next_cursor, None);
    model.apply(BridgeEvent::SessionDeleted("a".into()));
    assert_eq!(model.sessions.items[0].session_id.0.as_ref(), "b");
    model.apply(BridgeEvent::SessionList {
        sessions: vec![],
        next_cursor: None,
        first_page: true,
    });
    assert!(model.sessions.items.is_empty());
}

#[test]
fn config_replaces_and_config_mode_beats_legacy() {
    let mut model = Model::default();
    start(&mut model);
    assert!(matches!(model.next_mode(), Some(UiCmd::SetMode(id)) if id.0.as_ref() == "code"));
    let mode = SessionConfigOption::select(
        "mode",
        "Mode",
        "plan",
        vec![
            SessionConfigSelectOption::new("plan", "Plan"),
            SessionConfigSelectOption::new("build", "Build"),
        ],
    )
    .category(SessionConfigOptionCategory::Mode);
    model.apply(BridgeEvent::ConfigOptions(vec![mode]));
    assert!(
        matches!(model.next_mode(), Some(UiCmd::SetConfigOption { config_id, value })
        if config_id.0.as_ref() == "mode" && value.as_value_id().is_some_and(|v| v.0.as_ref() == "build"))
    );
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![
            SessionConfigOption::boolean("safe", "Safe", true),
        ])),
    )));
    assert_eq!(
        model
            .session
            .as_ref()
            .unwrap()
            .config_options
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(model.next_mode(), Some(UiCmd::SetMode(_))));
    assert!(
        matches!(prefs::change(&model, Some("safe"), &json!(false)), Some(UiCmd::SetConfigOption { value, .. }) if value.as_bool() == Some(false))
    );
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::CurrentModeUpdate(
            agent_client_protocol::schema::v1::CurrentModeUpdate::new("code"),
        ),
    )));
    assert_eq!(
        model
            .session
            .as_ref()
            .unwrap()
            .modes
            .as_ref()
            .unwrap()
            .current_mode_id
            .0
            .as_ref(),
        "code"
    );
    model.apply(BridgeEvent::ModeChanged("ask".into()));
    assert!(matches!(model.next_mode(), Some(UiCmd::SetMode(id)) if id.0.as_ref() == "code"));
}

#[test]
fn omitted_info_keeps_title_null_clears_it_and_commands_replace() {
    let mut model = Model::default();
    start(&mut model);
    let title: SessionInfoUpdate =
        serde_json::from_value(json!({"title": "Current", "updatedAt": "2026-10-08T10:00:00Z"}))
            .unwrap();
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::SessionInfoUpdate(title),
    )));
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::SessionInfoUpdate(SessionInfoUpdate::new()),
    )));
    assert_eq!(
        model.session.as_ref().unwrap().title.as_deref(),
        Some("Current")
    );
    let clear: SessionInfoUpdate =
        serde_json::from_value(json!({"title": null, "updatedAt": null})).unwrap();
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::SessionInfoUpdate(clear),
    )));
    assert!(model.session.as_ref().unwrap().title.is_none());
    assert!(model.session.as_ref().unwrap().updated_at.is_none());
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
            AvailableCommand::new("plan", "Plan"),
        ])),
    )));
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![])),
    )));
    assert!(model.sessions.commands.is_empty());
}

#[test]
fn palette_filter_and_selection_distinguish_client_from_agent_commands() {
    let mut model = Model::default();
    start(&mut model);
    model.sessions.commands = vec![AvailableCommand::new("plan", "Agent planner")];
    let mut picker = Palette::new(Kind::Commands);
    picker.query = "pla".into();
    assert!(matches!(
        picker.select("client:new", &model, true),
        Choice::Changed
    ));
    assert!(
        matches!(picker.select("agent:plan", &model, true), Choice::Insert(text) if text == "/plan ")
    );
    picker.query = "new".into();
    assert!(matches!(
        picker.select("client:new", &model, true),
        Choice::Command(UiCmd::NewSession)
    ));
    let mut caps = AgentCapabilities::default();
    caps.load_session = true;
    model.capabilities = caps;
    assert!(!model.can_open_sessions());
}

#[test]
fn session_picker_load_resume_delete_and_page_cursor() {
    use agent_client_protocol::schema::v1::{
        SessionDeleteCapabilities, SessionListCapabilities, SessionResumeCapabilities,
    };
    use tern_sdk::keys::Key;
    let mut model = Model::default();
    let caps = &mut model.capabilities;
    caps.load_session = true;
    caps.session_capabilities.list = Some(SessionListCapabilities::new());
    caps.session_capabilities.resume = Some(SessionResumeCapabilities::new());
    caps.session_capabilities.delete = Some(SessionDeleteCapabilities::new());
    model.apply(BridgeEvent::SessionList {
        sessions: vec![SessionInfo::new("saved", "/tmp/project")],
        next_cursor: Some("next".into()),
        first_page: true,
    });
    let mut picker = Palette::new(Kind::Sessions);
    assert!(matches!(
        picker.select("session:saved", &model, true),
        Choice::Command(UiCmd::LoadSession(id)) if id.0.as_ref() == "saved"
    ));
    assert!(matches!(
        picker.key(&Key::named("r"), &model),
        Choice::Command(UiCmd::ResumeSession(id)) if id.0.as_ref() == "saved"
    ));
    assert!(matches!(
        picker.key(&Key::named("d"), &model),
        Choice::Changed
    ));
    assert!(matches!(
        picker.key(&Key::named("d"), &model),
        Choice::Command(UiCmd::DeleteSession(id)) if id.0.as_ref() == "saved"
    ));
    assert!(matches!(
        picker.select("more", &model, true),
        Choice::Command(UiCmd::ListSessions { cursor: Some(cursor) }) if cursor == "next"
    ));
}

#[test]
fn hidden_usage_turn_captures_text_without_transcript_or_turn_totals() {
    use agent_client_protocol::schema::v1::{PromptResponse, StopReason};
    let mut model = Model::default();
    model.apply(BridgeEvent::Initialized(Box::new(
        agent_client_protocol::schema::v1::InitializeResponse::new(
            agent_client_protocol::schema::ProtocolVersion::V1,
        )
        .agent_info(agent_client_protocol::schema::v1::Implementation::new(
            "@agentclientprotocol/codex-acp",
            "2.1.1",
        )),
    )));
    start(&mut model);
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
            AvailableCommand::new("status", "Show status"),
        ])),
    )));
    model.begin_usage();
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "/status",
        )))),
    )));
    model.apply(BridgeEvent::Update(Box::new(
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "**5h limit:** 92% left (resets 14:00)",
        )))),
    )));
    model.apply(BridgeEvent::TurnEnded(Ok(Box::new(PromptResponse::new(
        StopReason::EndTurn,
    )))));
    assert!(model.transcript.is_empty());
    assert!(model.turn_summaries.is_empty());
    assert_eq!(model.usage_report.windows[0].used_pct, 8.0);
    assert!(matches!(model.turn, acp_tsp::model::Turn::Idle));
}

#[test]
fn prompt_typed_during_a_usage_query_runs_as_its_own_turn() {
    use agent_client_protocol::schema::v1::{PromptResponse, StopReason};
    let ended = || BridgeEvent::TurnEnded(Ok(Box::new(PromptResponse::new(StopReason::EndTurn))));
    let chunk = |text: &str| {
        BridgeEvent::Update(Box::new(SessionUpdate::AgentMessageChunk(
            ContentChunk::new(ContentBlock::Text(TextContent::new(text))),
        )))
    };
    let mut model = Model::default();
    model.begin_usage();
    model.apply(chunk("**Weekly limit:** 50% left"));
    // Queued behind the hidden query; the ACP side sends it once the query ends.
    model.submitted("real prompt".into());
    model.apply(ended());
    assert!(matches!(model.turn, acp_tsp::model::Turn::Running { .. }));
    model.apply(chunk("answer"));
    model.apply(ended());
    assert!(matches!(model.turn, acp_tsp::model::Turn::Idle));
    // The answer is in the transcript, the usage text is not, and only the real turn counts.
    let agent_text: Vec<_> = model
        .transcript
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Agent { blocks, .. } => Some(format!("{blocks:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(agent_text.len(), 1);
    assert!(agent_text[0].contains("answer"));
    assert_eq!(model.turn_summaries.len(), 1);
}

#[test]
fn usage_query_only_uses_a_command_the_agent_advertises() {
    use agent_client_protocol::schema::{
        ProtocolVersion,
        v1::{Implementation, InitializeResponse},
    };
    let model_for = |name: &str, commands: &[&str]| {
        let mut model = Model::default();
        model.apply(BridgeEvent::Initialized(Box::new(
            InitializeResponse::new(ProtocolVersion::V1).agent_info(Implementation::new(name, "1")),
        )));
        model.apply(BridgeEvent::Update(Box::new(
            SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(
                commands
                    .iter()
                    .map(|command| AvailableCommand::new(*command, ""))
                    .collect(),
            )),
        )));
        model
    };
    // Unadvertised commands would reach the model as an ordinary prompt.
    assert_eq!(
        model_for("@agentclientprotocol/claude-agent-acp", &[]).usage_command(),
        None
    );
    assert_eq!(
        model_for("@agentclientprotocol/claude-agent-acp", &["usage"]).usage_command(),
        Some("/usage")
    );
    assert_eq!(
        model_for("@agentclientprotocol/codex-acp", &["status", "review"]).usage_command(),
        Some("/status")
    );
    assert_eq!(model_for("other-agent", &["status"]).usage_command(), None);
}
