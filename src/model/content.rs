//! State for rich ACP content and client-side terminal lifecycles.
use std::{collections::HashMap, path::PathBuf};

use crate::event::TerminalEvent;
use agent_client_protocol::schema::v1::{
    ContentBlock, TerminalExitStatus, TerminalId, UsageUpdate,
};

const OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct TerminalBuffer {
    pub command: String,
    pub cwd: PathBuf,
    pub output: String,
    pub truncated: bool,
    pub status: Option<TerminalExitStatus>,
    pub released: bool,
}

#[derive(Debug, Default)]
pub struct ContentState {
    pub terminals: HashMap<TerminalId, TerminalBuffer>,
    pub blobs: HashMap<u64, String>,
    pub usage: Option<UsageUpdate>,
    #[cfg(feature = "unstable_end_turn_token_usage")]
    pub last_turn_usage: Option<agent_client_protocol::schema::v1::Usage>,
    #[cfg(feature = "unstable_session_notices")]
    pub notices: Vec<agent_client_protocol::schema::v1::Notice>,
}

impl ContentState {
    pub fn terminal(&mut self, event: TerminalEvent) {
        match event {
            TerminalEvent::Created { id, command, cwd } => {
                self.terminals.insert(
                    id,
                    TerminalBuffer {
                        command,
                        cwd,
                        output: String::new(),
                        truncated: false,
                        status: None,
                        released: false,
                    },
                );
            }
            TerminalEvent::Output {
                id,
                chunk,
                truncated,
            } => {
                if let Some(terminal) = self.terminals.get_mut(&id) {
                    terminal.output.push_str(&chunk);
                    terminal.truncated |= truncated;
                    if terminal.output.len() > OUTPUT_LIMIT {
                        let mut start = terminal.output.len() - OUTPUT_LIMIT;
                        while !terminal.output.is_char_boundary(start) {
                            start += 1;
                        }
                        terminal.output.drain(..start);
                        terminal.truncated = true;
                    }
                }
            }
            TerminalEvent::Exited { id, status } => {
                if let Some(terminal) = self.terminals.get_mut(&id) {
                    terminal.status = Some(status);
                }
            }
            TerminalEvent::Released(id) => {
                if let Some(terminal) = self.terminals.get_mut(&id) {
                    terminal.released = true;
                }
            }
        }
    }
    #[cfg(feature = "unstable_end_turn_token_usage")]
    pub fn turn_usage(&mut self, current: &agent_client_protocol::schema::v1::Usage) -> agent_client_protocol::schema::v1::Usage {
        use agent_client_protocol::schema::v1::Usage;
        let prior = self.last_turn_usage.replace(current.clone());
        if let Some(prior) = prior {
            Usage::new(current.total_tokens.saturating_sub(prior.total_tokens),
                current.input_tokens.saturating_sub(prior.input_tokens),
                current.output_tokens.saturating_sub(prior.output_tokens))
        } else {
            current.clone()
        }
    }
}

/// Stream text in place; preserve non-text blocks as separate keyed entries.
pub fn append_text(blocks: &mut Vec<ContentBlock>, block: ContentBlock) {
    match block {
        ContentBlock::Text(text) => {
            if let Some(ContentBlock::Text(last)) = blocks.last_mut() {
                last.text.push_str(&text.text);
            } else {
                blocks.push(ContentBlock::Text(text));
            }
        }
        block => blocks.push(block),
    }
}

#[cfg(feature = "unstable_plan_operations")]
pub fn plan_id(
    plan: &agent_client_protocol::schema::v1::PlanUpdateContent,
) -> Option<&agent_client_protocol::schema::v1::PlanId> {
    use agent_client_protocol::schema::v1::PlanUpdateContent;
    match plan {
        PlanUpdateContent::Items(items) => Some(&items.plan_id),
        PlanUpdateContent::File(file) => Some(&file.plan_id),
        PlanUpdateContent::Markdown(markdown) => Some(&markdown.plan_id),
        _ => None,
    }
}

#[cfg(any(
    feature = "unstable_plan_operations",
    feature = "unstable_session_compaction",
    feature = "unstable_subagents"
))]
impl super::Model {
    #[cfg(feature = "unstable_plan_operations")]
    pub(super) fn plan_update(
        &mut self,
        plan: agent_client_protocol::schema::v1::PlanUpdateContent,
    ) {
        let id = plan_id(&plan);
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            matches!(&entry.kind,
            super::EntryKind::IdentifiedPlan(old) if plan_id(old) == id)
        }) {
            entry.kind = super::EntryKind::IdentifiedPlan(plan);
        } else {
            self.push(super::EntryKind::IdentifiedPlan(plan));
        }
    }

    #[cfg(feature = "unstable_session_compaction")]
    pub(super) fn compaction_update(
        &mut self,
        update: agent_client_protocol::schema::v1::CompactionUpdate,
    ) {
        use agent_client_protocol::schema::MaybeUndefined;
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            matches!(&entry.kind,
            super::EntryKind::Compaction { id, .. } if id == &update.compaction_id)
        }) {
            if let super::EntryKind::Compaction {
                status,
                summary,
                error,
                ..
            } = &mut entry.kind
            {
                *status = update.status;
                match update.summary {
                    MaybeUndefined::Undefined => {}
                    MaybeUndefined::Null => summary.clear(),
                    MaybeUndefined::Value(value) => *summary = value,
                }
                match update.error {
                    MaybeUndefined::Undefined => {}
                    MaybeUndefined::Null => *error = None,
                    MaybeUndefined::Value(value) => *error = Some(value),
                }
            }
        } else {
            self.push(super::EntryKind::Compaction {
                id: update.compaction_id,
                status: update.status,
                summary: match update.summary {
                    MaybeUndefined::Value(value) => value,
                    _ => Vec::new(),
                },
                error: match update.error {
                    MaybeUndefined::Value(value) => Some(value),
                    _ => None,
                },
            });
        }
    }

    #[cfg(feature = "unstable_subagents")]
    pub(super) fn subagent_update(
        &mut self,
        update: agent_client_protocol::schema::v1::SubagentUpdate,
    ) {
        use agent_client_protocol::schema::MaybeUndefined;
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            matches!(&entry.kind,
            super::EntryKind::Subagent { id, .. } if id == &update.session_id)
        }) {
            if let super::EntryKind::Subagent {
                title,
                description,
                state,
                ..
            } = &mut entry.kind
            {
                match update.title {
                    MaybeUndefined::Value(value) => *title = value,
                    MaybeUndefined::Null => *title = update.session_id.to_string(),
                    MaybeUndefined::Undefined => {}
                }
                match update.description {
                    MaybeUndefined::Value(value) => *description = Some(value),
                    MaybeUndefined::Null => *description = None,
                    MaybeUndefined::Undefined => {}
                }
                match update.state {
                    MaybeUndefined::Value(value) => *state = Some(value),
                    MaybeUndefined::Null => *state = None,
                    MaybeUndefined::Undefined => {}
                }
            }
        } else {
            let title = match update.title {
                MaybeUndefined::Value(value) => value,
                _ => update.session_id.to_string(),
            };
            self.push(super::EntryKind::Subagent {
                id: update.session_id,
                title,
                description: match update.description {
                    MaybeUndefined::Value(value) => Some(value),
                    _ => None,
                },
                state: match update.state {
                    MaybeUndefined::Value(value) => Some(value),
                    _ => None,
                },
                messages: Vec::new(),
            });
        }
    }

    #[cfg(feature = "unstable_subagents")]
    pub(super) fn session_message(
        &mut self,
        message: agent_client_protocol::schema::v1::SessionMessage,
    ) {
        for entry in &mut self.transcript {
            if let super::EntryKind::Subagent { id, messages, .. } = &mut entry.kind
                && (message.sender_session_id.as_ref() == Some(id)
                    || message.recipient_session_id.as_ref() == Some(id))
            {
                if let Some(existing) = messages
                    .iter_mut()
                    .find(|old| old.message_id == message.message_id)
                {
                    if !message.content.is_undefined() {
                        existing.content = message.content;
                    }
                    if message.sender_session_id.is_some() {
                        existing.sender_session_id = message.sender_session_id;
                    }
                    if message.recipient_session_id.is_some() {
                        existing.recipient_session_id = message.recipient_session_id;
                    }
                } else {
                    messages.push(message);
                }
                return;
            }
        }
    }

    #[cfg(feature = "unstable_subagents")]
    pub(super) fn session_message_chunk(
        &mut self,
        chunk: agent_client_protocol::schema::v1::SessionMessageChunk,
    ) {
        use agent_client_protocol::schema::MaybeUndefined;
        for entry in &mut self.transcript {
            if let super::EntryKind::Subagent { id, messages, .. } = &mut entry.kind
                && (chunk.sender_session_id.as_ref() == Some(id)
                    || chunk.recipient_session_id.as_ref() == Some(id)
                    || messages.iter().any(|m| m.message_id == chunk.message_id))
            {
                if let Some(message) = messages
                    .iter_mut()
                    .find(|m| m.message_id == chunk.message_id)
                {
                    if let MaybeUndefined::Value(blocks) = &mut message.content {
                        append_text(blocks, chunk.content);
                    } else {
                        message.content = MaybeUndefined::Value(vec![chunk.content]);
                    }
                } else {
                    messages.push(
                        agent_client_protocol::schema::v1::SessionMessage::new(chunk.message_id)
                            .sender_session_id(chunk.sender_session_id)
                            .recipient_session_id(chunk.recipient_session_id)
                            .content(vec![chunk.content]),
                    );
                }
                return;
            }
        }
    }
}
