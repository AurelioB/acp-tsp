//! Whole-view rendering from session state; no terminal I/O.

use agent_client_protocol::schema::v1::{
    ContentBlock, PlanEntryStatus, SessionConfigOptionCategory, ToolCallContent, ToolCallStatus,
    ToolKind,
};
use tern_sdk::{
    Capabilities,
    ui::{self, ChecklistItem, Hunk, ItemStatus, Node, Phase, Status, View, html},
};

use super::{Msg, auth, composer::Composer, prefs};
use crate::model::{EntryKind, Model, Turn};

pub fn view(model: &Model, composer: &Composer, _caps: &Capabilities) -> View<Msg> {
    let mut main = ui::col().key("transcript");
    for entry in &model.transcript {
        let key = format!("entry.{}", entry.id);
        let node: Node<Msg> = match &entry.kind {
            EntryKind::User { text, .. } => ui::card()
                .key(key)
                .tone("user")
                .child(ui::md(text.clone()).key("body"))
                .into(),
            EntryKind::Agent { text, .. } => ui::md(text.clone())
                .key(key)
                .stream(model.is_streaming(entry))
                .into(),
            EntryKind::Thought { text, .. } => ui::section()
                .key(key)
                .head("Thinking")
                .collapsible(true)
                .collapsed(!model.is_streaming(entry))
                .child(ui::md(text.clone()).key("body").tone("muted"))
                .into(),
            EntryKind::Tool { call, cancelled } => {
                // Tern chooses its tool glyph from `name`; normalize ACP kinds
                // rather than relying on agent-specific programmatic names.
                let name = match call.kind {
                    ToolKind::Read => "read",
                    ToolKind::Edit => "edit",
                    ToolKind::Delete => "delete",
                    ToolKind::Move => "move",
                    ToolKind::Search => "search",
                    ToolKind::Execute => "execute",
                    ToolKind::Think => "think",
                    ToolKind::Fetch => "fetch",
                    ToolKind::SwitchMode => "switch_mode",
                    _ => call.name.as_deref().unwrap_or("other"),
                };
                let status = if *cancelled {
                    Status::Cancelled
                } else {
                    match call.status {
                        ToolCallStatus::Pending => Status::Pending,
                        ToolCallStatus::InProgress => Status::Running,
                        ToolCallStatus::Completed => Status::Done,
                        ToolCallStatus::Failed => Status::Error,
                        _ => Status::Pending,
                    }
                };
                let mut tool = ui::tool(name)
                    .key(key)
                    .title(call.title.clone())
                    .status(status)
                    .collapsible(true);
                if let Some(location) = call.locations.first() {
                    tool = tool.target(location.path.display().to_string());
                }
                for (i, content) in call.content.iter().enumerate() {
                    let body: Node<Msg> = match content {
                        ToolCallContent::Diff(diff) => {
                            let lines = diff
                                .old_text
                                .as_deref()
                                .unwrap_or_default()
                                .lines()
                                .map(|line| format!("-{line}"))
                                .chain(diff.new_text.lines().map(|line| format!("+{line}")))
                                .collect();
                            ui::diff()
                                .key(i)
                                .path(diff.path.display().to_string())
                                .hunks(vec![Hunk {
                                    old_start: Some(1),
                                    new_start: Some(1),
                                    lines,
                                }])
                                .into()
                        }
                        ToolCallContent::Content(content) => match &content.content {
                            ContentBlock::Text(text) => ui::md(text.text.clone()).key(i).into(),
                            ContentBlock::ResourceLink(link) => {
                                ui::md(crate::model::resource_link_markdown(&link.name, &link.uri))
                                    .key(i)
                                    .into()
                            }
                            _ => ui::code("Unsupported tool content").key(i).into(),
                        },
                        ToolCallContent::Terminal(terminal) => {
                            ui::code(format!("Terminal {}", terminal.terminal_id))
                                .key(i)
                                .into()
                        }
                        _ => ui::code("Unsupported tool content").key(i).into(),
                    };
                    tool = tool.child(body);
                }
                if let Some(raw) = &call.raw_input {
                    tool = tool.child(
                        ui::section()
                            .key("raw.in")
                            .head("Input")
                            .collapsible(true)
                            .collapsed(true)
                            .child(ui::code(raw.to_string()).key("body")),
                    );
                }
                if let Some(raw) = &call.raw_output {
                    tool = tool.child(
                        ui::section()
                            .key("raw.out")
                            .head("Output")
                            .collapsible(true)
                            .collapsed(true)
                            .child(ui::code(raw.to_string()).key("body")),
                    );
                }
                tool.into()
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
    let mut bar = ui::status()
        .key("bar")
        .child(ui::seg(title).key("agent"))
        .child(ui::seg(state).key("state"));
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
    if !model.status.is_empty() {
        bar = bar.child(ui::seg(model.status.clone()).key("status"));
    }
    let blocked =
        model.oldest_permission().is_some() || model.auth.is_some() || model.exited.is_some();
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
    if let Some(permission) = model.oldest_permission() {
        let id = permission.id;
        let title = permission
            .request
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Tool permission".into());
        let mut dialog = ui::card().key("permission").head(title);
        for (i, option) in permission.request.options.iter().enumerate() {
            dialog = dialog.child(
                html::button(format!("{}. {}", i + 1, option.name))
                    .key(format!("option.{i}"))
                    .on_click(Msg::Permission(id, option.option_id.clone())),
            );
        }
        result = result.layer(ui::col().child(dialog));
    } else if let Some(auth) = &model.auth {
        result = result.layer(auth::picker(auth, model.auth_selected));
    } else if let Some(reason) = &model.exited {
        result = result.layer(
            ui::col().child(
                ui::card()
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
