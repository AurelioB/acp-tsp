//! Interactive ACP elicitation forms and URL requests.
use std::collections::BTreeMap;

use super::Msg;
use crate::event::{PendingId, UiCmd};
use agent_client_protocol::schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationAcceptAction,
    ElicitationAction, ElicitationContentValue as Value, ElicitationMode,
    ElicitationPropertySchema as Property, ElicitationSchema, MultiSelectItems, StringFormat,
};
use tern_sdk::{
    ui::{self, Node, html},
    wire::{Change, Edit},
};

pub fn first_field(request: &CreateElicitationRequest) -> Option<String> {
    let ElicitationMode::Form(form) = &request.mode else {
        return None;
    };
    form.requested_schema
        .properties
        .iter()
        .find_map(|(name, property)| {
            match property {
                Property::String(p) if p.enum_values.is_none() && p.one_of.is_none() => Some(name),
                Property::Number(_) | Property::Integer(_) => Some(name),
                _ => None,
            }
            .map(|name| format!("layer.elicitation.form.{name}.field.{name}"))
        })
}

#[derive(Default)]
pub struct FormState {
    pub id: Option<PendingId>,
    pub values: BTreeMap<String, Value>,
    pub errors: BTreeMap<String, String>,
    pub focus: Option<String>,
    pub cursor: usize,
    pub option_cursor: usize,
}

impl FormState {
    pub fn init(&mut self, id: PendingId, request: &CreateElicitationRequest) {
        if self.id == Some(id) {
            return;
        }
        *self = Self {
            id: Some(id),
            ..Self::default()
        };
        if let ElicitationMode::Form(form) = &request.mode {
            for (name, property) in &form.requested_schema.properties {
                let value = match property {
                    Property::String(p) => p.default.clone().map(Value::String),
                    Property::Number(p) => p.default.map(Value::Number),
                    Property::Integer(p) => p.default.map(Value::Integer),
                    Property::Boolean(p) => Some(Value::Boolean(p.default.unwrap_or(false))),
                    Property::Array(p) => p.default.clone().map(Value::StringArray),
                    _ => None,
                };
                if let Some(value) = value {
                    self.values.insert(name.clone(), value);
                }
            }
        }
    }

    pub fn edit(&mut self, edit: &Edit, schema: &ElicitationSchema) -> bool {
        let Some(name) = edit
            .id
            .strip_prefix("layer.elicitation.form.")
            .and_then(|id| id.rsplit_once(".field.").map(|(_, name)| name))
        else {
            return false;
        };
        let Some(property) = schema.properties.get(name) else {
            return false;
        };
        if !matches!(
            property,
            Property::String(_) | Property::Integer(_) | Property::Number(_)
        ) {
            return false;
        }
        let mut text = self
            .values
            .get(name)
            .map_or(String::new(), |value| match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Integer(n) => n.to_string(),
                _ => String::new(),
            });
        let boundary = |utf16: usize| {
            text.char_indices()
                .scan(0, |n, (byte, c)| {
                    let start = *n;
                    *n += c.len_utf16();
                    Some((start, byte))
                })
                .chain(std::iter::once((text.encode_utf16().count(), text.len())))
                .find_map(|(offset, byte)| (offset == utf16).then_some(byte))
        };
        if edit.len != text.encode_utf16().count() || edit.from > edit.to {
            return false;
        }
        let (Some(from), Some(to)) = (boundary(edit.from), boundary(edit.to)) else {
            return false;
        };
        if edit.cursor > edit.len - (edit.to - edit.from) + edit.text.encode_utf16().count() {
            return false;
        }
        text.replace_range(from..to, &edit.text);
        self.values.insert(name.to_owned(), Value::String(text));
        self.focus = Some(name.to_owned());
        self.cursor = edit.cursor;
        self.errors.remove(name);
        true
    }

    pub fn change(&mut self, change: &Change, schema: &ElicitationSchema) -> bool {
        let Some(name) = change.name.as_deref() else {
            return false;
        };
        let Some(property) = schema.properties.get(name) else {
            return false;
        };
        match property {
            Property::Boolean(_) => {
                self.values.insert(
                    name.to_owned(),
                    Value::Boolean(change.checked.unwrap_or(false)),
                );
            }
            Property::String(p) if p.enum_values.is_some() || p.one_of.is_some() => {
                if let Some(value) = change.value.as_str() {
                    self.values
                        .insert(name.to_owned(), Value::String(value.to_owned()));
                }
            }
            Property::Array(_) => {
                let Some(value) = change.value.as_str() else {
                    return false;
                };
                let mut selected = match self.values.remove(name) {
                    Some(Value::StringArray(items)) => items,
                    _ => Vec::new(),
                };
                selected.retain(|item| item != value);
                if change.checked == Some(true) {
                    selected.push(value.to_owned());
                }
                self.values
                    .insert(name.to_owned(), Value::StringArray(selected));
            }
            _ => return false,
        }
        self.errors.remove(name);
        true
    }
    pub fn choose(&mut self, schema: &ElicitationSchema, name: &str, value: &str) -> bool {
        let Some(property) = schema.properties.get(name) else {
            return false;
        };
        match property {
            Property::String(p) => {
                let valid = p
                    .enum_values
                    .as_ref()
                    .is_some_and(|choices| choices.iter().any(|item| item == value))
                    || p.one_of
                        .as_ref()
                        .is_some_and(|choices| choices.iter().any(|item| item.value == value));
                if !valid {
                    return false;
                }
                self.values.insert(name.into(), Value::String(value.into()));
            }
            Property::Array(p) => {
                let valid = match &p.items {
                    MultiSelectItems::String(items) => {
                        items.values.iter().any(|item| item == value)
                    }
                    MultiSelectItems::Titled(items) => {
                        items.options.iter().any(|item| item.value == value)
                    }
                    _ => false,
                };
                if !valid {
                    return false;
                }
                let selected = self
                    .values
                    .entry(name.into())
                    .or_insert_with(|| Value::StringArray(Vec::new()));
                let Value::StringArray(selected) = selected else {
                    return false;
                };
                if let Some(index) = selected.iter().position(|item| item == value) {
                    selected.remove(index);
                } else {
                    selected.push(value.into());
                }
            }
            _ => return false,
        }
        self.errors.remove(name);
        true
    }

    /// Arrow keys move through a choice field; Space toggles a multi-choice row.
    pub fn option_key(&mut self, schema: &ElicitationSchema, key: &tern_sdk::keys::Key) -> bool {
        let Some((name, property)) = schema.properties.iter().find(|(_, property)| {
            matches!(property, Property::String(p) if p.enum_values.is_some() || p.one_of.is_some())
                || matches!(property, Property::Array(_))
        }) else {
            return false;
        };
        let values = match property {
            Property::String(p) => p
                .one_of
                .as_ref()
                .map(|choices| choices.len())
                .or_else(|| p.enum_values.as_ref().map(Vec::len))
                .unwrap_or(0),
            Property::Array(p) => match &p.items {
                MultiSelectItems::String(items) => items.values.len(),
                MultiSelectItems::Titled(items) => items.options.len(),
                _ => 0,
            },
            _ => 0,
        };
        if values == 0 {
            return false;
        }
        let moving = key.is("up") || key.is("down");
        if key.is("up") {
            self.option_cursor = self.option_cursor.saturating_sub(1);
        } else if key.is("down") {
            self.option_cursor = (self.option_cursor + 1).min(values - 1);
        } else if !key.is("space") && !key.is("enter") {
            return false;
        }
        let value = match property {
            Property::String(p) => p
                .one_of
                .as_ref()
                .and_then(|items| {
                    items
                        .get(self.option_cursor)
                        .map(|item| item.value.as_str())
                })
                .or_else(|| {
                    p.enum_values
                        .as_ref()
                        .and_then(|items| items.get(self.option_cursor).map(String::as_str))
                }),
            Property::Array(p) => match &p.items {
                MultiSelectItems::String(items) => {
                    items.values.get(self.option_cursor).map(String::as_str)
                }
                MultiSelectItems::Titled(items) => items
                    .options
                    .get(self.option_cursor)
                    .map(|item| item.value.as_str()),
                _ => None,
            },
            _ => None,
        };
        let Some(value) = value else { return false };
        if matches!(property, Property::Array(_)) && !key.is("space") {
            return moving;
        }
        if key.is("enter") && self.values.contains_key(name) {
            return false;
        }
        self.choose(schema, name, value)
    }

    pub fn accept(&mut self, id: PendingId, request: &CreateElicitationRequest) -> Option<UiCmd> {
        if self.id != Some(id) {
            return None;
        }
        let content = match &request.mode {
            ElicitationMode::Form(form) => {
                self.errors = validate(&form.requested_schema, &self.values);
                if !self.errors.is_empty() {
                    return None;
                }
                Some(normalized(&form.requested_schema, &self.values))
            }
            ElicitationMode::Url(_) => None,
            _ => return None,
        };
        Some(UiCmd::Elicitation {
            id,
            response: CreateElicitationResponse::new(ElicitationAction::Accept(
                ElicitationAcceptAction::new().content(content),
            )),
        })
    }
}

pub fn validate(
    schema: &ElicitationSchema,
    values: &BTreeMap<String, Value>,
) -> BTreeMap<String, String> {
    let mut errors = BTreeMap::new();
    for (name, property) in &schema.properties {
        let required = schema
            .required
            .as_ref()
            .is_some_and(|required| required.contains(name));
        let value = values.get(name);
        let blank = value.is_none_or(|v| {
            matches!(v, Value::String(s) if s.trim().is_empty())
                || matches!(v, Value::StringArray(items) if items.is_empty())
        });
        if blank {
            if required {
                errors.insert(name.clone(), "Required".into());
            }
            continue;
        }
        let error = match (property, value.unwrap()) {
            (Property::String(p), Value::String(s)) => {
                let size = s.chars().count();
                if p.min_length.is_some_and(|min| size < min as usize) {
                    Some("Too short")
                } else if p.max_length.is_some_and(|max| size > max as usize) {
                    Some("Too long")
                } else if p
                    .enum_values
                    .as_ref()
                    .is_some_and(|values| !values.contains(s))
                    || p.one_of
                        .as_ref()
                        .is_some_and(|options| !options.iter().any(|o| o.value == *s))
                {
                    Some("Choose a listed option")
                } else if p.format.is_some_and(|format| !valid_format(format, s)) {
                    Some("Invalid format")
                } else {
                    None
                }
            }
            (Property::Integer(p), v) => {
                let value = match v {
                    Value::Integer(n) => Some(*n),
                    Value::String(s) => s.parse::<i64>().ok(),
                    _ => None,
                };
                match value {
                    None => Some("Enter an integer"),
                    Some(n)
                        if p.minimum.is_some_and(|min| n < min)
                            || p.maximum.is_some_and(|max| n > max) =>
                    {
                        Some("Outside allowed range")
                    }
                    _ => None,
                }
            }
            (Property::Number(p), v) => {
                let value = match v {
                    Value::Number(n) => Some(*n),
                    Value::String(s) => s.parse::<f64>().ok(),
                    _ => None,
                };
                match value {
                    None => Some("Enter a number"),
                    Some(n) if !n.is_finite() => Some("Enter a finite number"),
                    Some(n)
                        if p.minimum.is_some_and(|min| n < min)
                            || p.maximum.is_some_and(|max| n > max) =>
                    {
                        Some("Outside allowed range")
                    }
                    _ => None,
                }
            }
            (Property::Boolean(_), Value::Boolean(_)) => None,
            (Property::Array(p), Value::StringArray(items)) => {
                if p.min_items.is_some_and(|min| items.len() < min as usize)
                    || p.max_items.is_some_and(|max| items.len() > max as usize)
                {
                    Some("Choose an allowed number of options")
                } else if !items.iter().all(|item| match &p.items {
                    MultiSelectItems::String(choices) => choices.values.contains(item),
                    MultiSelectItems::Titled(choices) => {
                        choices.options.iter().any(|o| &o.value == item)
                    }
                    _ => false,
                }) {
                    Some("Choose a listed option")
                } else {
                    None
                }
            }
            _ => Some("Invalid value"),
        };
        if let Some(error) = error {
            errors.insert(name.clone(), error.into());
        }
    }
    errors
}

fn valid_format(format: StringFormat, text: &str) -> bool {
    match format {
        StringFormat::Email => text.split_once('@').is_some_and(|(user, host)| {
            !user.is_empty() && host.contains('.') && !host.contains('@')
        }),
        StringFormat::Uri => text.split_once(':').is_some_and(|(scheme, rest)| {
            !scheme.is_empty() && !rest.is_empty() && !text.chars().any(char::is_whitespace)
        }),
        StringFormat::Date | StringFormat::DateTime => {
            let bytes = text.as_bytes();
            let date = bytes.len() >= 10
                && bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes
                    .iter()
                    .take(10)
                    .enumerate()
                    .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
            if matches!(format, StringFormat::Date) {
                date && bytes.len() == 10
            } else {
                date && bytes.get(10) == Some(&b'T') && text[11..].contains(':')
            }
        }
        _ => true,
    }
}

pub fn normalized(
    schema: &ElicitationSchema,
    values: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    values
        .iter()
        .filter_map(|(name, value)| {
            let property = schema.properties.get(name)?;
            let normalized = match (property, value) {
                (Property::Integer(_), Value::String(text)) => Value::Integer(text.parse().ok()?),
                (Property::Number(_), Value::String(text)) => Value::Number(text.parse().ok()?),
                _ => value.clone(),
            };
            Some((name.clone(), normalized))
        })
        .collect()
}

fn action(label: &str, key: &str, message: Msg, primary: bool) -> Node<Msg> {
    let mut row = ui::row()
        .key(label.to_lowercase())
        .role("omp.btn")
        .prop("gap", "xs")
        .on_click(message)
        .child(ui::text(label));
    if !key.is_empty() {
        row = row.child(ui::kbd([key]));
    }
    if primary {
        row.tone("accent").into()
    } else {
        row.into()
    }
}

pub fn dialog(id: PendingId, request: &CreateElicitationRequest, state: &FormState) -> Node<Msg> {
    // Bottom ask sheet: the transcript stays visible while the user answers.
    let mut card = ui::overlay()
        .anchor(ui::Anchor::Bottom)
        .size(ui::OverlaySize::Lg)
        .key("elicitation")
        .role("omp.editor")
        .head("Agent question")
        .child(
            ui::md(request.message.clone())
                .key("message")
                .role("omp.ask.question"),
        );
    match &request.mode {
        ElicitationMode::Url(url) => {
            card = card.child(
                ui::md(crate::model::resource_link_markdown("Open link", &url.url)).key("url"),
            );
            card = card.child(
                ui::row()
                    .key("actions")
                    .role("omp.actions")
                    .child(ui::spacer().prop("grow", 1))
                    .child(action("Cancel", "escape", Msg::ElicitCancel(id), false))
                    .child(action("Done", "enter", Msg::ElicitAccept(id), true)),
            );
        }
        ElicitationMode::Form(form) => {
            let mut fields = html::form().key("form");
            for (name, property) in &form.requested_schema.properties {
                let label = match property {
                    Property::String(p) => p.title.as_deref(),
                    Property::Integer(p) => p.title.as_deref(),
                    Property::Number(p) => p.title.as_deref(),
                    Property::Boolean(p) => p.title.as_deref(),
                    Property::Array(p) => p.title.as_deref(),
                    _ => None,
                }
                .unwrap_or(name);
                let mut field = html::div()
                    .key(name.clone())
                    .child(html::p(label).key("label"));
                match property {
                    Property::String(p) if p.enum_values.is_some() || p.one_of.is_some() => {
                        let choices: Vec<_> = p
                            .one_of
                            .as_ref()
                            .map(|options| {
                                options
                                    .iter()
                                    .map(|o| (o.value.clone(), o.title.clone()))
                                    .collect()
                            })
                            .unwrap_or_else(|| {
                                p.enum_values
                                    .clone()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|s| (s.clone(), s))
                                    .collect()
                            });
                        let mut options = ui::list()
                            .key("options")
                            .role("omp.ask.options")
                            .selected(format!(
                                "layer.elicitation.form.{name}.options.option.{}",
                                state.option_cursor
                            ));
                        for (index, (value, title)) in choices.into_iter().enumerate() {
                            options = options.child(ui::item(title)
                                .key(format!("option.{index}"))
                                .role("omp.ask.option")
                                .icon(if matches!(state.values.get(name), Some(Value::String(s)) if s == &value) { "check" } else { "circle" })
                                .on_click(Msg::ElicitChoice(name.clone(), value)));
                        }
                        field = field.child(options);
                    }
                    Property::Array(p) => {
                        let choices: Vec<_> = match &p.items {
                            MultiSelectItems::String(items) => items
                                .values
                                .iter()
                                .map(|v| (v.clone(), v.clone()))
                                .collect(),
                            MultiSelectItems::Titled(items) => items
                                .options
                                .iter()
                                .map(|o| (o.value.clone(), o.title.clone()))
                                .collect(),
                            _ => Vec::new(),
                        };
                        let mut options = ui::list()
                            .key("options")
                            .role("omp.ask.options")
                            .selected(format!(
                                "layer.elicitation.form.{name}.options.option.{}",
                                state.option_cursor
                            ));
                        for (index, (value, title)) in choices.into_iter().enumerate() {
                            options = options.child(ui::item(title)
                                .key(format!("option.{index}"))
                                .role("omp.ask.check")
                                .icon(if matches!(state.values.get(name), Some(Value::StringArray(items)) if items.contains(&value)) { "check" } else { "square" })
                                .on_click(Msg::ElicitChoice(name.clone(), value)));
                        }
                        field = field.child(options);
                    }
                    Property::Boolean(_) => {
                        field = field.child(
                            html::label()
                                .key("boolean")
                                .child(
                                    html::checkbox(name, "true")
                                        .checked(matches!(
                                            state.values.get(name),
                                            Some(Value::Boolean(true))
                                        ))
                                        .on_change(|change| Msg::ElicitChange(change.clone())),
                                )
                                .child(html::span(label)),
                        );
                    }
                    Property::String(_) | Property::Integer(_) | Property::Number(_) => {
                        let text = state.values.get(name).map_or(String::new(), |v| match v {
                            Value::String(s) => s.clone(),
                            Value::Integer(n) => n.to_string(),
                            Value::Number(n) => n.to_string(),
                            _ => String::new(),
                        });
                        let cursor = if state.focus.as_deref() == Some(name) {
                            state.cursor
                        } else {
                            text.encode_utf16().count()
                        };
                        field = field.child(
                            ui::input()
                                .key(format!("field.{name}"))
                                .text(text)
                                .cursor(cursor)
                                .placeholder(label)
                                .on_edit(|edit| Msg::ElicitEdit(edit.clone()))
                                .on_focus(Msg::ElicitFocus(name.clone())),
                        );
                    }
                    _ => field = field.child(html::p("Unsupported field type")),
                }
                if let Some(error) = state.errors.get(name) {
                    field = field.child(html::p(error.clone()).key("error"));
                }
                fields = fields.child(field);
            }
            card = card.child(fields).child(
                ui::row()
                    .key("actions")
                    .role("omp.actions")
                    .prop("gap", "sm")
                    .child(ui::spacer().prop("grow", 1))
                    .child(action("Decline", "", Msg::ElicitDecline(id), false))
                    .child(action("Cancel", "escape", Msg::ElicitCancel(id), false))
                    .child(action("Accept", "enter", Msg::ElicitAccept(id), true)),
            );
        }
        _ => card = card.child(ui::text("Unsupported elicitation mode")),
    }
    ui::col().child(card).into()
}
