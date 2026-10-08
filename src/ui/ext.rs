//! Adapter-specific tool metadata and controls. Account details never enter the default dock.
use agent_client_protocol::schema::v1::{ToolCall, ToolCallContent, ToolCallStatus, ToolKind};
use tern_sdk::ui::{self, Control, Hunk, Node, PrefsRow, PrefsSection, Status, html};

use super::Msg;
use crate::model::{EntryKind, Model};

pub fn tool(
    call: &ToolCall,
    cancelled: bool,
    key: String,
    model: &Model,
    depth: usize,
) -> Node<Msg> {
    let meta = model.ext.tools.get(&call.tool_call_id);
    let claude_name = meta.and_then(|m| m.tool_name.as_deref());
    let name = match claude_name {
        Some("Bash" | "PowerShell") => "execute",
        Some("Read") => "read",
        Some("Edit" | "Write" | "NotebookEdit") => "edit",
        Some("Glob" | "Grep") => "search",
        Some("WebFetch" | "WebSearch") => "fetch",
        Some("Task" | "Agent") => "think",
        Some("ExitPlanMode") => "switch_mode",
        Some("TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet") => "think",
        _ => match call.kind {
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
        },
    };
    let status = if cancelled {
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
    let mut node = ui::tool(name)
        .key(key)
        .title(call.title.clone())
        .status(status)
        .collapsible(true);
    if let Some(location) = call.locations.first() {
        node = node.target(location.path.display().to_string());
    }
    if let Some(meta) = meta {
        if meta.mcp {
            node = node.child(ui::badge("MCP").key("mcp"));
        }
        if let Some(cwd) = &meta.cwd {
            node = node.child(ui::text(cwd.clone()).key("cwd"));
        }
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
            ToolCallContent::Content(content) => {
                super::content::block(&content.content, format!("{i}"), model)
            }
            // Adapter terminal metadata is independent of client-side `terminal/*`; it supplies
            // the output of a terminal content reference without a terminal/create request.
            ToolCallContent::Terminal(terminal)
                if meta.is_some_and(|m| {
                    m.terminal_id.as_deref() == Some(terminal.terminal_id.0.as_ref())
                }) =>
            {
                continue;
            }
            ToolCallContent::Terminal(terminal) => super::content::terminal(
                &terminal.terminal_id,
                model.content.terminals.get(&terminal.terminal_id),
                format!("{i}"),
            ),
            _ => ui::code("Unsupported tool content").key(i).into(),
        };
        node = node.child(body);
    }
    if let Some(meta) = meta {
        if meta.terminal_id.is_some() {
            node = node.child(ui::ansi(meta.output.clone()).key("terminal.output"));
            if meta.truncated {
                node = node.child(ui::text("Earlier output truncated").key("terminal.truncated"));
            }
            if let Some(code) = meta.exit_code {
                node = node.child(ui::badge(format!("Exit {code}")).key("terminal.exit"));
            } else if let Some(signal) = &meta.signal {
                node = node.child(ui::badge(format!("Signal {signal}")).key("terminal.exit"));
            }
        }
        if !meta.mcp_output.is_empty() {
            node = node.child(ui::ansi(meta.mcp_output.clone()).key("mcp.output"));
        }
        if let Some(error) = &meta.error {
            node = node.child(
                ui::card()
                    .key("codex.error")
                    .tone("error")
                    .child(ui::text(error.clone()).key("message")),
            );
        }
    }
    if let Some(raw) = &call.raw_input {
        node = node.child(
            ui::section()
                .key("raw.in")
                .head("Input")
                .collapsible(true)
                .collapsed(true)
                .child(ui::code(raw.to_string()).key("body")),
        );
    }
    if let Some(raw) = &call.raw_output {
        node = node.child(
            ui::section()
                .key("raw.out")
                .head("Output")
                .collapsible(true)
                .collapsed(true)
                .child(ui::code(raw.to_string()).key("body")),
        );
    }
    // Claude emits parentToolUseId on child tool calls; only nest when the parent exists.
    // Each child is rendered once, in wire order, inside an indented agent node.
    if depth < 8 {
        let children: Vec<_> = model
            .transcript
            .iter()
            .filter_map(|entry| {
                let EntryKind::Tool {
                    call: child,
                    cancelled,
                } = &entry.kind
                else {
                    return None;
                };
                (model
                    .ext
                    .tools
                    .get(&child.tool_call_id)
                    .and_then(|m| m.parent.as_deref())
                    == Some(call.tool_call_id.0.as_ref()))
                .then_some((entry.id, child.as_ref(), *cancelled))
            })
            .collect();
        if !children.is_empty() {
            let mut agent = ui::agent("Agent")
                .key("subagent")
                .depth((depth + 1) as u32)
                .collapsible(true);
            for (id, child, cancelled) in children {
                agent = agent.child(tool(
                    child,
                    cancelled,
                    format!("child.{id}"),
                    model,
                    depth + 1,
                ));
            }
            node = node.child(agent);
        }
    }
    node.into()
}

pub fn plan_text(
    request: &agent_client_protocol::schema::v1::RequestPermissionRequest,
) -> Option<&str> {
    if request.tool_call.fields.kind != Some(ToolKind::SwitchMode) {
        return None;
    }
    let id = request.tool_call.tool_call_id.0.as_ref();
    let is_codex = id.starts_with("plan-review:")
        && request
            .options
            .iter()
            .any(|option| option.option_id.0.as_ref() == "implement_plan");
    let is_claude = request
        .options
        .iter()
        .any(|option| option.option_id.0.starts_with("exit-plan-"));
    if !is_codex && !is_claude {
        return None;
    }
    request
        .tool_call
        .fields
        .raw_input
        .as_ref()
        .and_then(|value| value.get("plan"))
        .and_then(serde_json::Value::as_str)
}

pub fn goal_form(model: &Model) -> Node<Msg> {
    let Some(goal) = &model.ext.goal else {
        return ui::col().into();
    };
    let mut rows = vec![PrefsRow {
        id: "objective".into(),
        label: Some("Objective".into()),
        control: Some(Control::Text {
            value: model.ext.goal_draft.clone(),
            placeholder: Some("What should the agent achieve?".into()),
            secret: None,
            mono: None,
        }),
        ..PrefsRow::default()
    }];
    for action in &goal.actions {
        rows.push(PrefsRow {
            id: action.clone(),
            label: Some(action.clone()),
            control: Some(Control::Action {
                act: Some(action.clone()),
                label: Some(if action == "set" {
                    "Set objective".into()
                } else {
                    action.clone()
                }),
            }),
            ..PrefsRow::default()
        });
    }
    ui::col()
        .child(
            ui::prefs()
                .key("goal")
                .title("Goal")
                .sections(vec![PrefsSection {
                    id: Some("goal".into()),
                    title: Some("Goal control".into()),
                    rows,
                    ..PrefsSection::default()
                }])
                .on_change(|change| Msg::GoalChange(change.value.clone()))
                .on_action("set", Msg::GoalAction("set".into()))
                .on_action("pause", Msg::GoalAction("pause".into()))
                .on_action("resume", Msg::GoalAction("resume".into()))
                .on_action("clear", Msg::GoalAction("clear".into())),
        )
        .into()
}

pub fn account_details(model: &Model) -> Node<Msg> {
    let mut overlay = ui::overlay()
        .modal(true)
        .key("account.details")
        .head("Account details");
    if let Some(status) = &model.ext.auth {
        if let Some(label) = &status.label {
            overlay = overlay.child(ui::text(label.clone()).key("label"));
        }
        if let Some(detail) = &status.detail {
            overlay = overlay.child(ui::text(detail.clone()).key("detail"));
        }
        if let Some(account) = &status.account {
            if let Some(plan) = &account.plan {
                overlay = overlay.child(ui::text(format!("Plan: {plan}")).key("plan"));
            }
            if let Some(email) = &account.email {
                overlay = overlay.child(ui::text(email.clone()).key("email"));
            }
        }
    }
    ui::col()
        .child(
            overlay.child(
                html::button("Close")
                    .key("close")
                    .on_click(Msg::AccountDetails),
            ),
        )
        .into()
}
