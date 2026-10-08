//! Native TSP picker state for slash commands, sessions, and mode choices.

use agent_client_protocol::schema::v1::{
    SessionConfigKind, SessionConfigOptionCategory, SessionConfigOptionValue,
    SessionConfigSelectOptions, SessionId,
};
use tern_sdk::{
    keys::Key,
    ui::{self, PickerItem},
};

use crate::{
    event::UiCmd,
    model::{EntryKind, Model},
};

use super::Msg;
#[cfg(feature = "unstable_llm_providers")]
use super::providers::ProviderEditor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Commands,
    Sessions,
    Mode,
    Providers,
}

#[derive(Debug)]
pub struct Palette {
    pub kind: Kind,
    pub query: String,
    pub selected: usize,
    pub confirm: Option<String>,
    #[cfg(feature = "unstable_llm_providers")]
    pub(super) provider_editor: Option<ProviderEditor>,
}

#[derive(Debug)]
pub enum Choice {
    Changed,
    Close,
    Insert(String),
    Command(UiCmd),
    Open(Kind, Option<UiCmd>),
    Prefs,
    Quit,
}

#[derive(Clone, Debug)]
struct Row {
    id: String,
    label: String,
    detail: String,
}

impl Palette {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            query: String::new(),
            selected: 0,
            confirm: None,
            #[cfg(feature = "unstable_llm_providers")]
            provider_editor: None,
        }
    }

    fn rows(&self, model: &Model) -> Vec<Row> {
        let mut rows = Vec::new();
        match self.kind {
            Kind::Commands => {
                for command in &model.sessions.commands {
                    let hint = command
                        .input
                        .as_ref()
                        .map(|input| match input {
                            agent_client_protocol::schema::v1::AvailableCommandInput::Unstructured(input) => input.hint.as_str(),
                            _ => "",
                        })
                        .unwrap_or_default();
                    rows.push(Row {
                        id: format!("agent:{}", command.name),
                        label: format!("/{}", command.name),
                        detail: format!("{} {hint}", command.description),
                    });
                }
                for (name, detail, available) in [
                    ("new", "Start a fresh session", true),
                    (
                        "sessions",
                        "Browse saved sessions",
                        model.can_open_sessions(),
                    ),
                    (
                        "resume",
                        "Resume a saved session",
                        model.capabilities.session_capabilities.resume.is_some()
                            && model.capabilities.session_capabilities.list.is_some(),
                    ),
                    (
                        "close",
                        "Close this session",
                        model.session.is_some()
                            && model.capabilities.session_capabilities.close.is_some(),
                    ),
                    (
                        "delete",
                        "Delete a saved session",
                        model.capabilities.session_capabilities.delete.is_some()
                            && model.capabilities.session_capabilities.list.is_some(),
                    ),
                    (
                        "fork",
                        "Fork this session",
                        cfg!(feature = "unstable_session_fork") && can_fork(model),
                    ),
                    ("mode", "Choose a mode", model.next_mode().is_some()),
                    (
                        "config",
                        "Session configuration",
                        model
                            .session
                            .as_ref()
                            .and_then(|s| s.config_options.as_ref())
                            .is_some_and(|o| !o.is_empty()),
                    ),
                    (
                        "providers",
                        "Configure providers",
                        cfg!(feature = "unstable_llm_providers") && has_providers(model),
                    ),
                    (
                        "logout",
                        "Sign out",
                        model.capabilities.auth.logout.is_some(),
                    ),
                    ("restart", "Restart agent", true),
                    ("quit", "Quit", true),
                ] {
                    if available {
                        rows.push(Row {
                            id: format!("client:{name}"),
                            label: format!("/{name}"),
                            detail: detail.into(),
                        });
                    }
                }
            }
            Kind::Sessions => {
                for info in &model.sessions.items {
                    let first_prompt = model
                        .session
                        .as_ref()
                        .filter(|session| session.id == info.session_id)
                        .and_then(|_| {
                            model.transcript.iter().find_map(|entry| {
                                if let EntryKind::User { text, .. } = &entry.kind {
                                    text.lines().next()
                                } else {
                                    None
                                }
                            })
                        });
                    let title = info
                        .title
                        .as_deref()
                        .or(first_prompt)
                        .unwrap_or(info.session_id.0.as_ref());
                    let date = info
                        .updated_at
                        .as_deref()
                        .map(relative_time)
                        .unwrap_or_default();
                    rows.push(Row {
                        id: format!("session:{}", info.session_id),
                        label: title.into(),
                        detail: format!("{}  {date}", info.cwd.display()),
                    });
                }
                if model.sessions.next_cursor.is_some() {
                    rows.push(Row {
                        id: "more".into(),
                        label: "Load more".into(),
                        detail: "Next page".into(),
                    });
                }
            }
            Kind::Mode => {
                if let Some(session) = &model.session {
                    if let Some(option) = session.config_options.as_ref().and_then(|options| {
                        options
                            .iter()
                            .find(|o| matches!(o.category, Some(SessionConfigOptionCategory::Mode)))
                    }) {
                        if let SessionConfigKind::Select(select) = &option.kind {
                            let choices: Vec<_> = match &select.options {
                                SessionConfigSelectOptions::Ungrouped(items) => {
                                    items.iter().collect()
                                }
                                SessionConfigSelectOptions::Grouped(groups) => {
                                    groups.iter().flat_map(|group| &group.options).collect()
                                }
                                _ => Vec::new(),
                            };
                            rows.extend(choices.into_iter().map(|choice| Row {
                                id: format!("config:{}:{}", option.id, choice.value),
                                label: choice.name.clone(),
                                detail: choice.description.clone().unwrap_or_default(),
                            }));
                        }
                    } else if let Some(modes) = &session.modes {
                        rows.extend(modes.available_modes.iter().map(|mode| Row {
                            id: format!("mode:{}", mode.id),
                            label: mode.name.clone(),
                            detail: mode.description.clone().unwrap_or_default(),
                        }));
                    }
                }
            }
            Kind::Providers =>
            {
                #[cfg(feature = "unstable_llm_providers")]
                if let Some(providers) = &model.sessions.providers {
                    rows.extend(
                        providers
                            .providers
                            .iter()
                            .enumerate()
                            .map(|(i, provider)| Row {
                                id: format!("provider:{i}"),
                                label: provider.provider_id.to_string(),
                                detail: if provider.current.is_some() {
                                    "Active".into()
                                } else {
                                    "Disabled".into()
                                },
                            }),
                    );
                }
            }
        }
        let filter = self.query.trim_start_matches('/').to_lowercase();
        if !filter.is_empty() {
            // Name matches first (prefix, then substring), then description matches; stable
            // within a rank so agent order is kept.
            let rank = |row: &Row| {
                let label = row.label.trim_start_matches('/').to_lowercase();
                if label.starts_with(&filter) {
                    Some(0)
                } else if label.contains(&filter) {
                    Some(1)
                } else if row.detail.to_lowercase().contains(&filter) {
                    Some(2)
                } else {
                    None
                }
            };
            rows.retain(|row| rank(row).is_some());
            rows.sort_by_key(|row| rank(row));
        }
        rows
    }

    pub fn render(&self, model: &Model) -> ui::Node<Msg> {
        #[cfg(feature = "unstable_llm_providers")]
        if let Some(editor) = &self.provider_editor {
            return editor.render().into();
        }
        let rows = self.rows(model);
        let selected = rows.get(self.selected).map(|row| row.id.clone());
        let mut picker = ui::picker()
            .key("palette")
            .title(match self.kind {
                Kind::Commands => "Commands",
                Kind::Sessions => "Sessions",
                Kind::Mode => "Mode",
                Kind::Providers => "Providers",
            })
            .query(self.query.clone())
            .items(
                rows.iter()
                    .map(|row| {
                        let mut item = PickerItem::new(&row.id, row.label.clone());
                        item.detail = Some(row.detail.clone().into());
                        item
                    })
                    .collect::<Vec<_>>(),
            )
            .on_select(|id| Msg::PickerSelect(id.into()))
            .on_activate(|id| Msg::PickerActivate(id.into()));
        if let Some(selected) = selected {
            picker = picker.selected(selected);
        }
        if let Some(id) = &self.confirm {
            picker = picker.subtitle(format!(
                "Delete {id}? Press d again to confirm · Esc cancels"
            ));
        }
        picker.into()
    }

    pub fn key(&mut self, key: &Key, model: &Model) -> Choice {
        #[cfg(feature = "unstable_llm_providers")]
        if let Some(editor) = &mut self.provider_editor {
            if key.is("escape") {
                self.provider_editor = None;
                return Choice::Changed;
            }
            return editor.key(key).map_or(Choice::Changed, Choice::Command);
        }
        if key.is("escape") {
            if self.confirm.take().is_some() {
                return Choice::Changed;
            }
            return Choice::Close;
        }
        let count = self.rows(model).len();
        if key.is("up") {
            self.selected = self.selected.saturating_sub(1);
            return Choice::Changed;
        }
        if key.is("down") {
            self.selected = (self.selected + 1).min(count.saturating_sub(1));
            return Choice::Changed;
        }
        if key.is("enter") {
            return self.activate(model, false);
        }
        if self.kind == Kind::Sessions && key.is("r") {
            return self.activate(model, true);
        }
        if self.kind == Kind::Sessions && key.is("d") {
            return self.delete(model);
        }
        if key.is("backspace") {
            self.query.pop();
            self.selected = 0;
            return Choice::Changed;
        }
        if let Some(text) = key.typed() {
            self.query.push_str(text);
            self.selected = 0;
            return Choice::Changed;
        }
        Choice::Changed
    }

    pub fn select(&mut self, id: &str, model: &Model, activate: bool) -> Choice {
        #[cfg(feature = "unstable_llm_providers")]
        if self.provider_editor.is_some() {
            return Choice::Changed;
        }
        if let Some(i) = self.rows(model).iter().position(|row| row.id == id) {
            self.selected = i;
            if activate {
                self.activate(model, false)
            } else {
                Choice::Changed
            }
        } else {
            Choice::Changed
        }
    }

    fn delete(&mut self, model: &Model) -> Choice {
        if model.capabilities.session_capabilities.delete.is_none() {
            return Choice::Changed;
        }
        let Some(row) = self.rows(model).get(self.selected).cloned() else {
            return Choice::Changed;
        };
        let Some(id) = row.id.strip_prefix("session:") else {
            return Choice::Changed;
        };
        if self.confirm.as_deref() == Some(id) {
            self.confirm = None;
            Choice::Command(UiCmd::DeleteSession(SessionId::new(id.to_owned())))
        } else {
            self.confirm = Some(id.to_owned());
            Choice::Changed
        }
    }

    fn activate(&mut self, model: &Model, resume: bool) -> Choice {
        let Some(row) = self.rows(model).get(self.selected).cloned() else {
            return Choice::Changed;
        };
        if let Some(command) = row.id.strip_prefix("agent:") {
            return Choice::Insert(format!("/{command} "));
        }
        if let Some(id) = row.id.strip_prefix("session:") {
            let id = SessionId::new(id.to_owned());
            return if resume {
                if model.capabilities.session_capabilities.resume.is_some() {
                    Choice::Command(UiCmd::ResumeSession(id))
                } else {
                    Choice::Changed
                }
            } else {
                model
                    .open_session(id)
                    .map_or(Choice::Changed, Choice::Command)
            };
        }
        if row.id == "more" {
            return Choice::Command(UiCmd::ListSessions {
                cursor: model.sessions.next_cursor.clone(),
            });
        }
        if let Some(id) = row.id.strip_prefix("mode:") {
            return Choice::Command(UiCmd::SetMode(id.to_owned().into()));
        }
        if let Some(ids) = row.id.strip_prefix("config:")
            && let Some((id, value)) = ids.split_once(':')
        {
            return Choice::Command(UiCmd::SetConfigOption {
                config_id: id.to_owned().into(),
                value: SessionConfigOptionValue::value_id(value.to_owned()),
            });
        }
        #[cfg(feature = "unstable_llm_providers")]
        if let Some(index) = row
            .id
            .strip_prefix("provider:")
            .and_then(|s| s.parse::<usize>().ok())
            && let Some(provider) = model
                .sessions
                .providers
                .as_ref()
                .and_then(|list| list.providers.get(index))
        {
            self.provider_editor = Some(ProviderEditor::new(provider));
            return Choice::Changed;
        }
        match row.id.as_str() {
            "client:new" => Choice::Command(UiCmd::NewSession),
            "client:sessions" | "client:resume" | "client:delete" => {
                Choice::Open(Kind::Sessions, Some(UiCmd::ListSessions { cursor: None }))
            }
            "client:close" => Choice::Command(UiCmd::CloseSession),
            "client:fork" => Choice::Command(UiCmd::ForkSession),
            "client:config" => Choice::Prefs,
            "client:mode" => Choice::Open(Kind::Mode, None),
            "client:providers" => {
                #[cfg(feature = "unstable_llm_providers")]
                {
                    Choice::Open(Kind::Providers, Some(UiCmd::ListProviders))
                }
                #[cfg(not(feature = "unstable_llm_providers"))]
                {
                    Choice::Changed
                }
            }
            "client:logout" => Choice::Command(UiCmd::Logout),
            "client:restart" => Choice::Command(UiCmd::Restart),
            "client:quit" => Choice::Quit,
            _ => Choice::Changed,
        }
    }
}

#[cfg(feature = "unstable_session_fork")]
fn can_fork(model: &Model) -> bool {
    model.session.is_some() && model.capabilities.session_capabilities.fork.is_some()
}
#[cfg(not(feature = "unstable_session_fork"))]
fn can_fork(_: &Model) -> bool {
    false
}
#[cfg(feature = "unstable_llm_providers")]
fn has_providers(model: &Model) -> bool {
    model.capabilities.providers.is_some()
}
#[cfg(not(feature = "unstable_llm_providers"))]
fn has_providers(_: &Model) -> bool {
    false
}

// ISO 8601 UTC dates from session/list; a malformed or future timestamp is
// displayed verbatim rather than pretending to know its age.
fn relative_time(date: &str) -> String {
    let Some(day) = date.get(0..10).and_then(|day| {
        let year: i64 = day.get(0..4)?.parse().ok()?;
        let month: i64 = day.get(5..7)?.parse().ok()?;
        let date: i64 = day.get(8..10)?.parse().ok()?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&date) {
            return None;
        }
        let year = year - i64::from(month <= 2);
        let era = year.div_euclid(400);
        let yoe = year - era * 400;
        let mp = month + if month > 2 { -3 } else { 9 };
        Some(
            era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + date - 1 - 719468,
        )
    }) else {
        return date.into();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400;
    let days = now as i64 - day;
    if days < 0 {
        date.into()
    } else if days == 0 {
        "today".into()
    } else if days == 1 {
        "yesterday".into()
    } else if days < 30 {
        format!("{days}d ago")
    } else if days < 365 {
        format!("{}mo ago", days / 30)
    } else {
        format!("{}y ago", days / 365)
    }
}
