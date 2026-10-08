//! Session config rendered as a native TSP prefs sheet.

use agent_client_protocol::schema::v1::{
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory, SessionConfigOptionValue,
    SessionConfigSelectOptions,
};
use tern_sdk::{
    keys::Key,
    ui::{self, ChoiceOption, ChoiceStyle, Control, PrefsRow, PrefsSection},
};

use crate::{event::UiCmd, model::Model};

use super::Msg;

pub fn render(model: &Model) -> impl Into<ui::Node<Msg>> {
    let rows = model
        .session
        .as_ref()
        .and_then(|s| s.config_options.as_ref())
        .map(|options| options.iter().map(row).collect())
        .unwrap_or_default();
    ui::prefs()
        .key("config")
        .title("Session configuration")
        .sections(vec![PrefsSection {
            id: Some("options".into()),
            title: Some("Agent settings".into()),
            rows,
            ..PrefsSection::default()
        }])
        .on_change(|change| Msg::ConfigChange(change.item.clone(), change.value.clone()))
}

fn row(option: &SessionConfigOption) -> PrefsRow {
    let control = match &option.kind {
        SessionConfigKind::Boolean(boolean) => Some(Control::Switch {
            on: boolean.current_value,
        }),
        SessionConfigKind::Select(select) => {
            let options = match &select.options {
                SessionConfigSelectOptions::Ungrouped(items) => items
                    .iter()
                    .map(|item| ChoiceOption {
                        value: item.value.to_string(),
                        label: Some(item.name.clone()),
                        detail: item.description.clone(),
                    })
                    .collect(),
                SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .flat_map(|group| {
                        group.options.iter().map(|item| ChoiceOption {
                            value: item.value.to_string(),
                            label: Some(format!("{} · {}", group.name, item.name)),
                            detail: item.description.clone(),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Some(Control::Choice {
                value: select.current_value.to_string(),
                options,
                style: Some(ChoiceStyle::Menu),
                mono: None,
            })
        }
        _ => None,
    };
    PrefsRow {
        id: option.id.to_string(),
        label: Some(option.name.clone()),
        hint: option.description.clone(),
        control,
        ..PrefsRow::default()
    }
}

pub fn change(model: &Model, id: Option<&str>, value: &serde_json::Value) -> Option<UiCmd> {
    let option = model
        .session
        .as_ref()?
        .config_options
        .as_ref()?
        .iter()
        .find(|option| Some(option.id.0.as_ref()) == id)?;
    let value = match &option.kind {
        SessionConfigKind::Boolean(_) => SessionConfigOptionValue::boolean(value.as_bool()?),
        SessionConfigKind::Select(select) => {
            let value = value.as_str()?;
            let valid = match &select.options {
                SessionConfigSelectOptions::Ungrouped(items) => {
                    items.iter().any(|item| item.value.0.as_ref() == value)
                }
                SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .flat_map(|group| &group.options)
                    .any(|item| item.value.0.as_ref() == value),
                _ => false,
            };
            if !valid {
                return None;
            }
            SessionConfigOptionValue::value_id(value.to_owned())
        }
        _ => return None,
    };
    Some(UiCmd::SetConfigOption {
        config_id: option.id.clone(),
        value,
    })
}

pub fn key(key: &Key, model: &Model, selected: &mut usize) -> Option<UiCmd> {
    let options = model.session.as_ref()?.config_options.as_ref()?;
    if key.is("up") {
        *selected = selected.saturating_sub(1);
        return None;
    }
    if key.is("down") {
        *selected = (*selected + 1).min(options.len().saturating_sub(1));
        return None;
    }
    let option = options.get(*selected)?;
    if !(key.is("enter") || key.is("left") || key.is("right") || key.is("space")) {
        return None;
    }
    match &option.kind {
        SessionConfigKind::Boolean(current) => Some(UiCmd::SetConfigOption {
            config_id: option.id.clone(),
            value: SessionConfigOptionValue::boolean(!current.current_value),
        }),
        SessionConfigKind::Select(select) => {
            let choices: Vec<_> = match &select.options {
                SessionConfigSelectOptions::Ungrouped(items) => items.iter().collect(),
                SessionConfigSelectOptions::Grouped(groups) => {
                    groups.iter().flat_map(|group| &group.options).collect()
                }
                _ => Vec::new(),
            };
            if choices.is_empty() {
                return None;
            }
            let current = choices
                .iter()
                .position(|item| item.value == select.current_value)
                .unwrap_or(0);
            let index = if key.is("left") {
                (current + choices.len() - 1) % choices.len()
            } else {
                (current + 1) % choices.len()
            };
            Some(UiCmd::SetConfigOption {
                config_id: option.id.clone(),
                value: SessionConfigOptionValue::value_id(choices.get(index)?.value.clone()),
            })
        }
        _ => None,
    }
}

pub fn chip(model: &Model, category: SessionConfigOptionCategory) -> Option<String> {
    let option = model
        .session
        .as_ref()?
        .config_options
        .as_ref()?
        .iter()
        .find(|o| o.category.as_ref() == Some(&category))?;
    match &option.kind {
        SessionConfigKind::Select(select) => {
            let selected = match &select.options {
                SessionConfigSelectOptions::Ungrouped(items) => {
                    items.iter().find(|o| o.value == select.current_value)
                }
                SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .flat_map(|g| &g.options)
                    .find(|o| o.value == select.current_value),
                _ => None,
            };
            Some(format!(
                "{}: {}",
                option.name,
                selected.map_or(select.current_value.0.as_ref(), |s| &s.name)
            ))
        }
        SessionConfigKind::Boolean(value) => Some(format!(
            "{}: {}",
            option.name,
            if value.current_value { "On" } else { "Off" }
        )),
        _ => None,
    }
}
