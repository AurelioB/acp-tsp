//! Whole-view rendering from session state; no terminal I/O.

use agent_client_protocol::schema::v1::{
    ContentBlock, PlanEntryStatus, SessionConfigOptionCategory,
};
use tern_sdk::{
    Capabilities,
    ui::{self, ChecklistItem, ItemStatus, Node, Phase, View, html},
};

use super::ext;
use super::{Msg, auth, composer::Composer, prefs};
use crate::model::{EntryKind, Model, Turn};

/// Layer content as a child of the region: a region root draws no element of its own
/// (TSP), so a `picker`/`prefs`/`card` given as the root would be invisible.
pub fn layer(node: impl Into<Node<Msg>>) -> Node<Msg> {
    ui::col().child(node).into()
}

pub fn view(model: &Model, composer: &Composer, caps: &Capabilities) -> View<Msg> {
    view_with(
        model,
        composer,
        caps,
        &super::elicit::FormState::default(),
        None,
    )
}

pub fn view_with(
    model: &Model,
    composer: &Composer,
    _caps: &Capabilities,
    form: &super::elicit::FormState,
    picker: Option<&super::attach::AttachPicker>,
) -> View<Msg> {
    let mut main = ui::col().key("transcript");
    for entry in &model.transcript {
        let key = format!("entry.{}", entry.id);
        let node: Node<Msg> = match &entry.kind {
            EntryKind::User { blocks, .. } => {
                let mut card = ui::card().key(key).tone("user");
                for node in super::content::blocks(blocks, "body", model) {
                    card = card.child(node);
                }
                card.into()
            }
            EntryKind::Steering { blocks } => ui::card()
                .key(key)
                .tone("user")
                .child(ui::badge("Steering").key("label"))
                .children(super::content::blocks(blocks, "body", model))
                .into(),
            EntryKind::Agent { blocks, .. } => {
                if blocks.len() == 1 && matches!(blocks.first(), Some(ContentBlock::Text(_))) {
                    let ContentBlock::Text(text) = &blocks[0] else {
                        unreachable!()
                    };
                    ui::md(text.text.clone())
                        .key(key)
                        .stream(model.is_streaming(entry))
                        .into()
                } else {
                    ui::col()
                        .key(key)
                        .children(super::content::blocks(blocks, "body", model))
                        .into()
                }
            }
            EntryKind::Thought { blocks, .. } => ui::section()
                .key(key)
                .head("Thinking")
                .collapsible(true)
                .collapsed(!model.is_streaming(entry))
                .children(super::content::blocks(blocks, "body", model))
                .into(),
            EntryKind::Tool { call, cancelled } => {
                let parent = model
                    .ext
                    .tools
                    .get(&call.tool_call_id)
                    .and_then(|meta| meta.parent.as_deref());
                let nested = parent.is_some_and(|id| model.transcript.iter().any(|entry|
                    matches!(&entry.kind, EntryKind::Tool { call, .. } if call.tool_call_id.0.as_ref() == id)));
                if nested {
                    continue;
                }
                ext::tool(call, *cancelled, key, model, 0)
            }
            EntryKind::Plan(plan) => {
                let items = plan
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(i, task)| ChecklistItem {
                        id: format!("task.{i}"),
                        text: task.content.clone().into(),
                        status: Some(match task.status {
                            PlanEntryStatus::Pending => ItemStatus::Pending,
                            PlanEntryStatus::InProgress => ItemStatus::Active,
                            PlanEntryStatus::Completed => ItemStatus::Done,
                            _ => ItemStatus::Pending,
                        }),
                        note: None,
                    })
                    .collect();
                ui::checklist()
                    .key(key)
                    .phases(vec![Phase {
                        id: "plan".into(),
                        title: Some("Plan".into()),
                        items,
                        collapsed: None,
                    }])
                    .into()
            }
            #[cfg(feature = "unstable_plan_operations")]
            EntryKind::IdentifiedPlan(plan) => match plan {
                agent_client_protocol::schema::v1::PlanUpdateContent::Items(items) => {
                    let tasks = items
                        .entries
                        .iter()
                        .enumerate()
                        .map(|(i, task)| ChecklistItem {
                            id: format!("task.{i}"),
                            text: task.content.clone().into(),
                            status: Some(match task.status {
                                PlanEntryStatus::Pending => ItemStatus::Pending,
                                PlanEntryStatus::InProgress => ItemStatus::Active,
                                PlanEntryStatus::Completed => ItemStatus::Done,
                                _ => ItemStatus::Pending,
                            }),
                            note: None,
                        })
                        .collect();
                    ui::checklist()
                        .key(key)
                        .phases(vec![Phase {
                            id: items.plan_id.to_string(),
                            title: Some("Plan".into()),
                            items: tasks,
                            collapsed: None,
                        }])
                        .into()
                }
                agent_client_protocol::schema::v1::PlanUpdateContent::Markdown(markdown) => {
                    ui::md(markdown.content.clone()).key(key).into()
                }
                agent_client_protocol::schema::v1::PlanUpdateContent::File(file) => {
                    ui::md(crate::model::resource_link_markdown("Plan", &file.uri))
                        .key(key)
                        .into()
                }
                _ => ui::text("Unsupported plan").key(key).into(),
            },
            #[cfg(feature = "unstable_session_compaction")]
            EntryKind::Compaction {
                status,
                summary,
                error,
                ..
            } => {
                let mut section = ui::section()
                    .key(key)
                    .head("Context compacted")
                    .collapsible(true)
                    .collapsed(false);
                if matches!(
                    status,
                    agent_client_protocol::schema::v1::CompactionStatus::InProgress
                ) {
                    section = section.child(ui::spinner().key("progress").label("Compacting"));
                }
                for block in super::content::blocks(summary, "summary", model) {
                    section = section.child(block);
                }
                if let Some(error) = error {
                    section = section.child(ui::text(error.clone()).key("error"));
                }
                section.into()
            }
            #[cfg(feature = "unstable_subagents")]
            EntryKind::Subagent {
                title,
                description,
                state,
                messages,
                ..
            } => {
                use agent_client_protocol::schema::v1::StateUpdate;
                let status = match state {
                    Some(StateUpdate::Running(_)) => tern_sdk::ui::AgentStatus::Running,
                    Some(StateUpdate::RequiresAction(_)) => tern_sdk::ui::AgentStatus::Parked,
                    _ => tern_sdk::ui::AgentStatus::Idle,
                };
                let mut agent = ui::agent(title.clone())
                    .key(key)
                    .depth(1_u32)
                    .collapsible(true)
                    .status(status);
                if let Some(description) = description {
                    agent = agent.child(ui::text(description.clone()).key("description"));
                }
                for (index, message) in messages.iter().enumerate() {
                    if let agent_client_protocol::schema::MaybeUndefined::Value(blocks) =
                        &message.content
                    {
                        for block in
                            super::content::blocks(blocks, &format!("message.{index}"), model)
                        {
                            agent = agent.child(block);
                        }
                    }
                }
                agent.into()
            }
            #[cfg(feature = "unstable_end_turn_token_usage")]
            EntryKind::Usage(usage) => ui::text(format!(
                "Tokens: {} input · {} output · {} total",
                usage.input_tokens, usage.output_tokens, usage.total_tokens
            ))
            .key(key)
            .into(),
        };
        main = main.child(node);
    }
    for (i, error) in model.errors.iter().enumerate() {
        main = main.child(
            ui::card()
                .key(format!("error.{i}"))
                .tone("error")
                .child(ui::md(error.clone()).key("body")),
        );
    }
    for (i, notice) in model.notices.iter().enumerate() {
        main = main.child(
            ui::card()
                .key(format!("notice.{i}"))
                .tone("muted")
                .child(ui::md(notice.clone()).key("body")),
        );
    }
    #[cfg(feature = "unstable_session_notices")]
    for (i, notice) in model.content.notices.iter().enumerate() {
        let tone = match notice.severity {
            agent_client_protocol::schema::v1::NoticeSeverity::Warning => "warning",
            agent_client_protocol::schema::v1::NoticeSeverity::Error => "error",
            _ => "info",
        };
        let mut toast = ui::toast(notice.title.clone())
            .key(format!("agent.notice.{i}"))
            .tone(tone);
        if let Some(description) = &notice.description {
            toast = toast.sub(description.clone());
        }
        main = main.child(toast);
    }

    let title = model
        .agent
        .as_ref()
        .map(|agent| agent.title.as_deref().unwrap_or(&agent.name))
        .unwrap_or("Agent");
    let state = if model.exited.is_some() {
        "Exited"
    } else if model.session.is_some() {
        "Connected"
    } else {
        "Starting"
    };
    // Tern's status bar hides segments that don't fit, so keep it to what changes decisions:
    // the connection state only while it is not simply connected.
    let mut bar = ui::status().key("bar").child(ui::seg(title).key("agent"));
    if state != "Connected" {
        bar = bar.child(ui::seg(state).key("state"));
    }
    if let Some(session) = &model.session {
        if let Some(title) = &session.title {
            bar = bar.child(ui::seg(title.clone()).key("session"));
        }
        if let Some(mode) = prefs::chip(model, SessionConfigOptionCategory::Mode).or_else(|| {
            session.modes.as_ref().and_then(|modes| {
                modes
                    .available_modes
                    .iter()
                    .find(|mode| mode.id == modes.current_mode_id)
                    .map(|mode| mode.name.clone())
            })
        }) {
            bar = bar.child(ui::seg(mode).key("mode"));
        }
        for (key, category) in [
            ("model", SessionConfigOptionCategory::Model),
            ("effort", SessionConfigOptionCategory::ThoughtLevel),
        ] {
            if let Some(value) = prefs::chip(model, category) {
                bar = bar.child(ui::seg(value).key(key));
            }
        }
    }
    if let Turn::Running { since, cancelling } = model.turn {
        bar = bar
            .child(ui::seg(if cancelling { "Cancelling" } else { "Working" }).key("turn"))
            .child(ui::spinner().key("spinner"))
            .child(ui::elapsed().key("elapsed").since(since));
    }
    if model.queued > 0 {
        bar = bar.child(ui::seg(format!("{} queued", model.queued)).key("queue"));
    }
    if !model.status.is_empty()
        && model.status != "Ready"
        && !matches!(model.turn, Turn::Running { .. })
    {
        bar = bar.child(ui::seg(model.status.clone()).key("status"));
    }
    if let Some(auth) = &model.ext.auth
        && let Some(label) = &auth.label
    {
        bar = bar.child(
            html::button(label.clone())
                .key("account")
                .on_click(Msg::AccountDetails),
        );
    }
    // Both adapters report tokens spent, not a quota ceiling: a plain count, and only when the
    // context meter (which already counts tokens) is absent.
    if let Some(quota) = model.ext.quota_tokens
        && model.content.usage.is_none()
    {
        bar = bar.child(ui::seg(format!("{quota} tokens spent")).key("quota"));
    }
    if let Some(usage) = &model.content.usage {
        bar = bar.child(
            ui::meter()
                .key("context")
                .value(if usage.size == 0 {
                    0.0
                } else {
                    (usage.used as f64 / usage.size as f64).clamp(0.0, 1.0)
                })
                .label(format!("{} / {} tokens", usage.used, usage.size)),
        );
        if let Some(cost) = &usage.cost {
            bar = bar.child(ui::seg(format!("{} {:.2}", cost.currency, cost.amount)).key("cost"));
        }
    }
    if let Some(rate) = &model.ext.rate_limit {
        main = main.child(
            ui::card()
                .key("rate.limit")
                .tone("warning")
                .child(ui::text(rate.clone()).key("message")),
        );
    }
    if let Some(toast) = &model.ext.toast {
        main = main.child(ui::toast(toast.clone()).key("extension.result"));
    }
    let blocked = model.dialogs.front().is_some() || model.auth.is_some() || model.exited.is_some();
    let dock = ui::col().child(bar).child(
        ui::editor()
            .key("ed")
            .text(composer.text.clone())
            .cursor(composer.utf16_cursor())
            .placeholder("Message (Enter sends · Shift+Enter newline)")
            .readonly(blocked)
            .sendable(!blocked)
            .on_send(|text| Msg::Send(text.to_owned()))
            .on_edit(|edit| Msg::Edit(edit.clone()))
            .on_undo(Msg::Undo)
            .on_focus(Msg::Focus),
    );
    let mut result = View::new().main(main).dock(dock);
    if let Some(request) = model.oldest_elicitation() {
        // `elicit::dialog` already returns the region's `col`.
        result = result.layer(super::elicit::dialog(request.id, &request.request, form));
    } else if let Some(permission) = model.visible_permission() {
        result = result.layer(super::permission::dialog(
            permission.id,
            &permission.request,
        ));
    } else if let Some(auth) = &model.auth {
        result = result.layer(layer(auth::picker(auth, model.auth_selected)));
    } else if model.ext.account_details {
        result = result.layer(ext::account_details(model));
    } else if model.ext.goal_open {
        result = result.layer(ext::goal_form(model));
    } else if let Some(picker) = picker.filter(|p| p.open) {
        result = result.layer(layer(picker.view()));
    } else if let Some(reason) = &model.exited {
        result = result.layer(
            ui::col().child(
                ui::overlay()
                    .modal(true)
                    .key("exited")
                    .head("Agent exited")
                    .child(ui::md(reason.clone()).key("reason"))
                    .child(
                        ui::text(if model.restartable {
                            "Press r to restart or Ctrl+D to quit"
                        } else {
                            "Press Ctrl+D to quit"
                        })
                        .key("hint"),
                    ),
            ),
        );
    }
    result
}
