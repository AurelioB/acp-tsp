//! Native TSP picker state for slash commands, sessions, and mode choices.

use agent_client_protocol::schema::v1::{
    SessionConfigKind, SessionConfigOptionCategory, SessionConfigOptionValue,
    SessionConfigSelectOptions, SessionId,
};
use tern_sdk::{
    keys::Key,
    ui::{self, Anchor, OrderEntry, PickerAction, PickerItem},
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
    Model,
    Effort,
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
    Goal,
    Quit,
}

#[derive(Clone, Debug)]
struct Row {
    id: String,
    label: String,
    detail: String,
}

/// A client command typed or pasted in full (`/config`): what Enter does instead of sending
/// it as a prompt. An agent command of the same name wins, so agent commands always reach
/// the agent.
pub fn typed_client_command(text: &str, model: &Model) -> Option<Choice> {
    let name = text.trim().strip_prefix('/')?;
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    let mut palette = Palette::new(Kind::Commands);
    let rows = palette.rows(model);
    if rows.iter().any(|row| row.id == format!("agent:{name}")) {
        return None;
    }
    let id = format!("client:{name}");
    rows.iter()
        .any(|row| row.id == id)
        .then(|| palette.select(&id, model, true))
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
                        "model",
                        "Switch model for this session",
                        has_category(model, SessionConfigOptionCategory::Model),
                    ),
                    (
                        "effort",
                        "Choose thinking effort",
                        has_category(model, SessionConfigOptionCategory::ThoughtLevel),
                    ),
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
                    (
                        "goal",
                        "Control agent goal",
                        model.session.is_some() && model.ext.goal.is_some(),
                    ),
                ] {
                    if available {
                        rows.push(Row {
                            id: format!("client:{name}"),
                            label: format!("/{name}"),
                            detail: format!("acp-tsp · {detail}"),
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
            Kind::Mode | Kind::Model | Kind::Effort => {
                let category = match self.kind {
                    Kind::Mode => SessionConfigOptionCategory::Mode,
                    Kind::Model => SessionConfigOptionCategory::Model,
                    Kind::Effort => SessionConfigOptionCategory::ThoughtLevel,
                    _ => unreachable!(),
                };
                if let Some(session) = &model.session {
                    if let Some(option) = session.config_options.as_ref().and_then(|options| {
                        options
                            .iter()
                            .find(|o| o.category.as_ref() == Some(&category))
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
                    } else if self.kind == Kind::Mode
                        && let Some(modes) = &session.modes
                    {
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
            rows.sort_by_key(|row| {
                (
                    rank(row),
                    self.kind == Kind::Commands && row.id.starts_with("client:"),
                )
            });
        }
        rows
    }

    pub fn render(&self, model: &Model) -> ui::Node<Msg> {
        #[cfg(feature = "unstable_llm_providers")]
        if let Some(editor) = &self.provider_editor {
            return editor.render().into();
        }
        let rows = self.rows(model);
        if self.kind == Kind::Commands {
            let list = ui::list()
                .key("list")
                .selected(
                    rows.get(self.selected)
                        .map(|row| row.id.clone())
                        .unwrap_or_default(),
                )
                .empty("No items")
                .max_lines(10)
                .children(rows.iter().map(|row| {
                    ui::item(row.label.trim_start_matches('/'))
                        .key(row.id.clone())
                        .icon(if row.id.starts_with("agent:") {
                            "action"
                        } else {
                            "settings"
                        })
                        .detail(vec![ui::span(row.detail.clone()).style("muted")])
                        .value(if row.id.starts_with("client:") {
                            "acp-tsp"
                        } else {
                            ""
                        })
                }))
                .on_select(|id| Msg::PickerSelect(id.into()))
                .on_activate(|id| Msg::PickerActivate(id.into()));
            return ui::overlay()
                .key("complete")
                .role("omp.autocomplete")
                .anchor(Anchor::Caret("dock.ed".into()))
                .child(ui::col().role("omp.select").child(list))
                .into();
        }
        let mut order = Vec::new();
        let mut group = "";
        for row in &rows {
            let next = if row.id.starts_with("agent:") {
                "Agent commands"
            } else {
                "Client commands"
            };
            if self.kind == Kind::Commands && next != group {
                order.push(OrderEntry::Group {
                    group: next.into(),
                    label: next.into(),
                    count: None,
                });
                group = next;
            }
            order.push(OrderEntry::Item(row.id.clone()));
        }
        let selected = rows.get(self.selected).map(|row| row.id.clone());
        let (title, icon, noun) = match self.kind {
            Kind::Sessions => ("Sessions", "history", "sessions"),
            Kind::Mode => ("Mode", "settings", "modes"),
            Kind::Model => ("Models", "cpu", "models"),
            Kind::Effort => ("Thinking effort", "sparkles", "levels"),
            Kind::Providers => ("Providers", "globe", "providers"),
            Kind::Commands => unreachable!(),
        };
        let mut picker = ui::picker()
            .key("palette")
            .title(title)
            .icon(icon)
            .noun(noun)
            .size(ui::PickerSize::Lg)
            .layout(ui::PickerLayout::Rows)
            .preview(ui::PickerPreview::Side)
            .query(self.query.clone())
            .cursor(self.query.encode_utf16().count())
            .placeholder(format!("Search {noun}…"))
            .items(
                rows.iter()
                    .map(|row| {
                        let mut item = PickerItem::new(&row.id, row.label.clone());
                        item.detail = Some(row.detail.clone().into());
                        item
                    })
                    .collect::<Vec<_>>(),
            )
            .order(order)
            .actions(vec![
                PickerAction {
                    id: "choose".into(),
                    label: Some("Select".into()),
                    keys: vec!["enter".into()],
                    primary: Some(true),
                    ..PickerAction::default()
                },
                PickerAction {
                    id: "close".into(),
                    label: Some("Close".into()),
                    keys: vec!["esc".into()],
                    end: Some(true),
                    ..PickerAction::default()
                },
            ])
            .on_action("choose", Msg::PickerConfirm)
            .on_action("close", Msg::PickerClose)
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
        let preview = rows.get(self.selected).map(|row| {
            ui::col()
                .child(ui::text(row.label.clone()).role("omp.ask.question"))
                .child(ui::md(row.detail.clone()))
        });
        if let Some(preview) = preview {
            picker = picker.child(preview);
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
        if key.is("enter") || (self.kind == Kind::Commands && key.is("tab")) {
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

    pub fn activate(&mut self, model: &Model, resume: bool) -> Choice {
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
            "client:model" => Choice::Open(Kind::Model, None),
            "client:effort" => Choice::Open(Kind::Effort, None),
            "client:goal" => Choice::Goal,
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

fn has_category(model: &Model, category: SessionConfigOptionCategory) -> bool {
    model
        .session
        .as_ref()
        .and_then(|session| session.config_options.as_ref())
        .is_some_and(|options| {
            options
                .iter()
                .any(|option| option.category.as_ref() == Some(&category))
        })
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
