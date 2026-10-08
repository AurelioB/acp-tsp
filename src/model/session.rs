//! Session catalog, capability-aware mode selection, and session metadata.

use agent_client_protocol::schema::v1::{
    AvailableCommand, SessionConfigKind, SessionConfigOptionCategory, SessionConfigOptionValue,
    SessionId, SessionInfo, SessionUpdate,
};

use crate::event::UiCmd;

use super::Model;

#[derive(Debug, Default)]
pub struct SessionCatalog {
    pub items: Vec<SessionInfo>,
    pub next_cursor: Option<String>,
    pub commands: Vec<AvailableCommand>,
    #[cfg(feature = "unstable_llm_providers")]
    pub providers: Option<agent_client_protocol::schema::v1::ListProvidersResponse>,
}

impl Model {
    pub(super) fn apply_session_update(&mut self, update: SessionUpdate) -> Option<SessionUpdate> {
        match update {
            SessionUpdate::AvailableCommandsUpdate(update) => {
                self.sessions.commands = update.available_commands;
            }
            SessionUpdate::CurrentModeUpdate(update) => {
                if let Some(modes) = self
                    .session
                    .as_mut()
                    .and_then(|session| session.modes.as_mut())
                {
                    modes.current_mode_id = update.current_mode_id;
                }
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                if let Some(session) = &mut self.session {
                    session.config_options = Some(update.config_options);
                }
            }
            SessionUpdate::SessionInfoUpdate(update) => {
                if let Some(session) = &mut self.session {
                    if !update.title.is_undefined() {
                        session.title = update.title.take();
                    }
                    if !update.updated_at.is_undefined() {
                        session.updated_at = update.updated_at.take();
                    }
                }
            }
            other => return Some(other),
        }
        None
    }

    /// Prefer the modern mode config selector; legacy modes are a fallback only.
    pub fn next_mode(&self) -> Option<UiCmd> {
        let session = self.session.as_ref()?;
        if let Some(option) = session.config_options.as_ref().and_then(|options| {
            options
                .iter()
                .find(|option| matches!(option.category, Some(SessionConfigOptionCategory::Mode)))
        }) {
            if let SessionConfigKind::Select(select) = &option.kind {
                let choices: Vec<_> = match &select.options {
                    agent_client_protocol::schema::v1::SessionConfigSelectOptions::Ungrouped(
                        items,
                    ) => items.iter().collect(),
                    agent_client_protocol::schema::v1::SessionConfigSelectOptions::Grouped(
                        groups,
                    ) => groups.iter().flat_map(|group| &group.options).collect(),
                    _ => Vec::new(),
                };
                let next = choices
                    .iter()
                    .position(|item| item.value == select.current_value)
                    .map_or(0, |i| (i + 1) % choices.len());
                return choices.get(next).map(|item| UiCmd::SetConfigOption {
                    config_id: option.id.clone(),
                    value: SessionConfigOptionValue::value_id(item.value.clone()),
                });
            }
            return None;
        }
        let modes = session.modes.as_ref()?;
        let next = modes
            .available_modes
            .iter()
            .position(|mode| mode.id == modes.current_mode_id)
            .map_or(0, |i| (i + 1) % modes.available_modes.len());
        modes
            .available_modes
            .get(next)
            .map(|mode| UiCmd::SetMode(mode.id.clone()))
    }

    pub fn can_open_sessions(&self) -> bool {
        let cap = &self.capabilities.session_capabilities;
        cap.list.is_some() && (self.capabilities.load_session || cap.resume.is_some())
    }

    pub fn open_session(&self, id: SessionId) -> Option<UiCmd> {
        if self.capabilities.load_session {
            Some(UiCmd::LoadSession(id))
        } else if self.capabilities.session_capabilities.resume.is_some() {
            Some(UiCmd::ResumeSession(id))
        } else {
            None
        }
    }
}
