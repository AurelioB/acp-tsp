//! Pure session state, updated from [`crate::event::BridgeEvent`]s.

pub mod session;

use std::{collections::VecDeque, path::PathBuf, time::Instant};

use agent_client_protocol::schema::v1::{
    AgentCapabilities, AuthMethod, ContentBlock, ContentChunk, Implementation,
    PermissionOptionKind, Plan, RequestPermissionRequest, SessionConfigOption, SessionId,
    SessionModeState, SessionUpdate, StopReason, ToolCall, ToolCallId, ToolCallStatus,
};

use crate::event::{BridgeEvent, PendingId, SessionStartKind};
use session::SessionCatalog;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: u64,
    // Turn during which the entry was received (queued local prompts are
    // received in the preceding turn). Only tools use it for cancellation.
    turn_id: u64,
    pub kind: EntryKind,
}

#[derive(Debug, Clone)]
pub enum EntryKind {
    User {
        text: String,
        message_id: Option<String>,
        local: bool,
    },
    Agent {
        text: String,
        message_id: Option<String>,
    },
    Thought {
        text: String,
        message_id: Option<String>,
    },
    Tool {
        call: Box<ToolCall>,
        cancelled: bool,
    },
    Plan(Plan),
}

#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub id: PendingId,
    pub request: Box<RequestPermissionRequest>,
}

#[derive(Debug, Clone)]
pub struct AuthPrompt {
    pub methods: Vec<AuthMethod>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub id: SessionId,
    pub kind: SessionStartKind,
    pub cwd: PathBuf,
    pub additional_dirs: Vec<PathBuf>,
    pub modes: Option<SessionModeState>,
    pub config_options: Option<Vec<SessionConfigOption>>,
    pub title: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum Turn {
    Idle,
    Running { since: Instant, cancelling: bool },
}

#[derive(Debug)]
pub struct Model {
    pub status: String,
    pub agent: Option<Implementation>,
    pub auth: Option<AuthPrompt>,
    pub auth_selected: usize,
    pub capabilities: AgentCapabilities,
    pub sessions: SessionCatalog,
    pub session: Option<ActiveSession>,
    pub turn: Turn,
    pub queued: usize,
    pub transcript: Vec<Entry>,
    pub permissions: VecDeque<PendingPermission>,
    pub errors: Vec<String>,
    pub notices: Vec<String>,
    pub debug_updates: Vec<String>,
    pub exited: Option<String>,
    pub restartable: bool,
    next_id: u64,
    // Locally submitted prompts are authoritative; suppress only a matching
    // streamed echo during that prompt's live turn (not other user messages).
    local_echo: Option<(String, usize)>,
    queued_echoes: VecDeque<String>,
    active_turn_id: u64,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            status: String::new(),
            agent: None,
            capabilities: AgentCapabilities::default(),
            sessions: SessionCatalog::default(),
            auth: None,
            auth_selected: 0,
            session: None,
            turn: Turn::Idle,
            queued: 0,
            transcript: Vec::new(),
            permissions: VecDeque::new(),
            errors: Vec::new(),
            notices: Vec::new(),
            debug_updates: Vec::new(),
            exited: None,
            restartable: true,
            next_id: 0,
            local_echo: None,
            queued_echoes: VecDeque::new(),
            active_turn_id: 0,
        }
    }
}

impl Model {
    fn push(&mut self, kind: EntryKind) {
        self.next_id += 1;
        self.transcript.push(Entry {
            id: self.next_id,
            turn_id: self.active_turn_id,
            kind,
        });
    }

    /// Record a submitted prompt locally; ACP-side prompt queues are reflected in the dock.
    pub fn submitted(&mut self, text: String) {
        if matches!(self.turn, Turn::Idle) {
            self.active_turn_id += 1;
            self.turn = Turn::Running {
                since: Instant::now(),
                cancelling: false,
            };
            self.local_echo = Some((text.clone(), 0));
        } else {
            self.queued += 1;
            self.queued_echoes.push_back(text.clone());
        }
        self.push(EntryKind::User {
            text,
            message_id: None,
            local: true,
        });
    }

    pub fn cancel(&mut self) {
        if let Turn::Running { cancelling, .. } = &mut self.turn {
            *cancelling = true;
        }
    }

    /// A transcript entry is still receiving content in the current turn.
    pub fn is_streaming(&self, entry: &Entry) -> bool {
        matches!(self.turn, Turn::Running { .. }) && entry.turn_id == self.active_turn_id
    }

    pub fn apply(&mut self, event: BridgeEvent) {
        match event {
            BridgeEvent::Status(text) => self.status = text,
            BridgeEvent::Initialized(init) => {
                self.agent = init.agent_info;
                self.capabilities = init.agent_capabilities;
            }
            BridgeEvent::AuthRequired { methods, message } => {
                self.auth_selected = 0;
                self.auth = Some(AuthPrompt { methods, message });
            }
            BridgeEvent::SessionStarting(_) => {
                self.session = None;
                self.auth = None;
                self.turn = Turn::Idle;
                self.queued = 0;
                self.local_echo = None;
                self.queued_echoes.clear();
                self.transcript.clear();
                self.permissions.clear();
                self.errors.clear();
                self.notices.clear();
                self.debug_updates.clear();
                self.sessions.commands.clear();
                self.status = "Loading session…".into();
            }
            BridgeEvent::SessionStarted {
                session_id,
                kind,
                cwd,
                additional_dirs,
                modes,
                config_options,
            } => {
                self.session = Some(ActiveSession {
                    id: session_id,
                    kind,
                    cwd,
                    additional_dirs,
                    modes,
                    config_options,
                    title: None,
                    updated_at: None,
                });
                self.auth = None;
                self.exited = None;
                self.status = "Ready".into();
            }
            BridgeEvent::SessionClosed => {
                self.session = None;
                self.status = "Session closed".into();
            }
            BridgeEvent::SessionList {
                sessions,
                next_cursor,
                first_page,
            } => {
                if first_page {
                    self.sessions.items = sessions;
                } else {
                    self.sessions.items.extend(sessions);
                }
                self.sessions.next_cursor = next_cursor;
            }
            BridgeEvent::SessionDeleted(id) => {
                self.sessions
                    .items
                    .retain(|session| session.session_id != id);
            }
            BridgeEvent::ModeChanged(id) => {
                if let Some(modes) = self
                    .session
                    .as_mut()
                    .and_then(|session| session.modes.as_mut())
                {
                    modes.current_mode_id = id;
                }
            }
            BridgeEvent::ConfigOptions(options) => {
                if let Some(session) = &mut self.session {
                    session.config_options = Some(options);
                }
            }
            #[cfg(feature = "unstable_llm_providers")]
            BridgeEvent::Providers(providers) => self.sessions.providers = Some(*providers),
            BridgeEvent::LoggedOut => {
                self.auth = None;
                self.session = None;
                self.status = "Logged out".into();
            }
            BridgeEvent::Update(update) => self.update(*update),
            BridgeEvent::Permission { id, request } => {
                self.permissions
                    .push_back(PendingPermission { id, request });
            }
            BridgeEvent::Resolved(id) => self.permissions.retain(|permission| permission.id != id),
            BridgeEvent::TurnEnded(result) => {
                let result = result.map(|response| response.stop_reason);
                if matches!(result, Ok(StopReason::Cancelled)) {
                    for entry in &mut self.transcript {
                        if entry.turn_id == self.active_turn_id
                            && let EntryKind::Tool { call, cancelled } = &mut entry.kind
                            && matches!(
                                call.status,
                                ToolCallStatus::Pending | ToolCallStatus::InProgress
                            )
                        {
                            *cancelled = true;
                        }
                    }
                }
                match result {
                    Ok(StopReason::EndTurn) => {}
                    Ok(reason) => self.notices.push(format!("Turn ended: {reason:?}")),
                    Err(error) => self.errors.push(error),
                }
                self.local_echo = None;
                if self.queued > 0 {
                    self.queued -= 1;
                    self.local_echo = self.queued_echoes.pop_front().map(|text| (text, 0));
                    self.active_turn_id += 1;
                    self.turn = Turn::Running {
                        since: Instant::now(),
                        cancelling: false,
                    };
                } else {
                    self.turn = Turn::Idle;
                }
            }
            BridgeEvent::Error(error) => self.errors.push(error),
            BridgeEvent::AgentExited { reason } => {
                self.exited = Some(reason);
                self.turn = Turn::Idle;
                self.local_echo = None;
                self.queued_echoes.clear();
                self.permissions.clear();
                self.auth = None;
                self.queued = 0;
            }
            _ => {}
        }
    }

    fn chunk(&mut self, kind: &'static str, chunk: ContentChunk) {
        let text = match chunk.content {
            ContentBlock::Text(content) => content.text,
            ContentBlock::ResourceLink(link) => resource_link_markdown(&link.name, &link.uri),
            _ => {
                self.debug_updates
                    .push(format!("{kind}: unsupported content"));
                return;
            }
        };
        // Tern shows a locally submitted prompt immediately. Only discard
        // ACP user chunks matching the remaining prefix of that exact prompt.
        if kind == "user"
            && let Some((expected, consumed)) = &mut self.local_echo
        {
            if expected
                .get(*consumed..)
                .is_some_and(|remaining| remaining.starts_with(&text))
            {
                *consumed += text.len();
                return;
            }
            self.local_echo = None;
        }
        let message_id = chunk.message_id.map(|id| id.to_string());
        if let Some(last) = self.transcript.last_mut() {
            let target = match &mut last.kind {
                EntryKind::User {
                    text,
                    message_id: id,
                    local: false,
                } if kind == "user" && *id == message_id => Some(text),
                EntryKind::Agent {
                    text,
                    message_id: id,
                } if kind == "agent" && *id == message_id => Some(text),
                EntryKind::Thought {
                    text,
                    message_id: id,
                } if kind == "thought" && *id == message_id => Some(text),
                _ => None,
            };
            if let Some(target) = target {
                target.push_str(&text);
                return;
            }
        }
        self.push(match kind {
            "user" => EntryKind::User {
                text,
                message_id,
                local: false,
            },
            "agent" => EntryKind::Agent { text, message_id },
            _ => EntryKind::Thought { text, message_id },
        });
    }

    fn update(&mut self, update: SessionUpdate) {
        let Some(update) = self.apply_session_update(update) else {
            return;
        };
        match update {
            SessionUpdate::UserMessageChunk(chunk) => self.chunk("user", chunk),
            SessionUpdate::AgentMessageChunk(chunk) => self.chunk("agent", chunk),
            SessionUpdate::AgentThoughtChunk(chunk) => self.chunk("thought", chunk),
            SessionUpdate::ToolCall(call) => {
                if let Some(entry) = self.tool_mut(&call.tool_call_id) {
                    entry.kind = EntryKind::Tool {
                        call: Box::new(call),
                        cancelled: false,
                    };
                } else {
                    self.push(EntryKind::Tool {
                        call: Box::new(call),
                        cancelled: false,
                    });
                }
            }
            SessionUpdate::ToolCallUpdate(update) => {
                if let Some(entry) = self.tool_mut(&update.tool_call_id) {
                    if let EntryKind::Tool { call, .. } = &mut entry.kind {
                        call.update(update.fields);
                    }
                } else {
                    let mut call = ToolCall::new(update.tool_call_id, "Tool call");
                    call.update(update.fields);
                    self.push(EntryKind::Tool {
                        call: Box::new(call),
                        cancelled: false,
                    });
                }
            }
            SessionUpdate::Plan(plan) => {
                if let Some(entry) = self
                    .transcript
                    .iter_mut()
                    .rev()
                    .find(|entry| matches!(entry.kind, EntryKind::Plan(_)))
                {
                    entry.kind = EntryKind::Plan(plan);
                } else {
                    self.push(EntryKind::Plan(plan));
                }
            }
            other => self.debug_updates.push(
                (match &other {
                    SessionUpdate::AvailableCommandsUpdate(_) => "available_commands_update",
                    SessionUpdate::CurrentModeUpdate(_) => "current_mode_update",
                    SessionUpdate::ConfigOptionUpdate(_) => "config_option_update",
                    SessionUpdate::SessionInfoUpdate(_) => "session_info_update",
                    SessionUpdate::UsageUpdate(_) => "usage_update",
                    _ => "unstable_update",
                })
                .to_string(),
            ),
        }
    }

    fn tool_mut(&mut self, id: &ToolCallId) -> Option<&mut Entry> {
        self.transcript.iter_mut().rev().find(
            |entry| matches!(&entry.kind, EntryKind::Tool { call, .. } if call.tool_call_id == *id),
        )
    }

    pub fn oldest_permission(&self) -> Option<&PendingPermission> {
        self.permissions.front()
    }

    pub fn reject_once(&self) -> Option<agent_client_protocol::schema::v1::PermissionOptionId> {
        self.oldest_permission()?
            .request
            .options
            .iter()
            .find(|option| option.kind == PermissionOptionKind::RejectOnce)
            .map(|option| option.option_id.clone())
    }
}

/// Only Markdown nodes make Tern's file links interactive. Escape link labels
/// and destinations so agent-provided paths cannot change the link target.
pub(crate) fn resource_link_markdown(name: &str, uri: &str) -> String {
    let label = name
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    let target = uri
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)");
    format!("[{label}]({target})")
}
