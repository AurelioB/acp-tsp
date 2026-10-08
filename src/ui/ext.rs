//! Adapter-specific tool metadata and controls. Account details never enter the default dock.
use agent_client_protocol::schema::v1::{ToolCall, ToolCallContent, ToolCallStatus, ToolKind};
use tern_sdk::ui::{
    self, Control, Hunk, Node, PrefsRow, PrefsSection, Status, TargetKind, ToolFrame, ToolPreview,
    html,
};

use super::Msg;
use crate::model::{EntryKind, Model};

/// The verb omp shows for a tool, when the tool has a fixed one.
fn title_for(name: &str) -> Option<&'static str> {
    Some(match name {
        "bash" => "Bash",
        "read" => "Read",
        "edit" => "Edit",
        "write" => "Write",
        "glob" => "Glob",
        "grep" => "Grep",
        "find" => "Find",
        "fetch" => "Fetch",
        "web" => "Web",
        "task" => "Task",
        _ => return None,
    })
}

/// Tern's built-in tool cards are keyed by the `omp.tool.*` role and their head props.
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
        Some("Bash" | "PowerShell") => "bash",
        Some("Read") => "read",
        Some("Edit" | "NotebookEdit") => "edit",
        Some("Write") => "write",
        Some("Glob") => "glob",
        Some("Grep") => "grep",
        Some("WebFetch") => "fetch",
        Some("WebSearch") => "web",
        Some("Task" | "Agent") => "task",
        Some("TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet") => "task",
        _ if meta.is_some_and(|m| m.mcp) => "mcp",
        _ => match call.kind {
            ToolKind::Read => "read",
            ToolKind::Edit => "edit",
            ToolKind::Delete => "delete",
            ToolKind::Move => "move",
            ToolKind::Search => "find",
            ToolKind::Execute => "bash",
            ToolKind::Think => "task",
            ToolKind::Fetch => "fetch",
            ToolKind::SwitchMode => "switch_mode",
            _ => "generic",
        },
    };
    // Codex runs every shell command as `exec_command` and labels it by intent (`kind: read`
    // + "List files"), with no path: it is a command with output, not a file read.
    let name = if claude_name.is_none()
        && matches!(
            call.name.as_deref(),
            Some("exec_command" | "shell" | "local_shell" | "bash")
        ) {
        "bash"
    } else {
        name
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
    let finished = matches!(status, Status::Done | Status::Error | Status::Cancelled);
    let raw = call.raw_input.as_ref();
    let arg = |names: &[&str]| {
        names.iter().find_map(|name| {
            raw.and_then(|v| v.get(name))
                .and_then(serde_json::Value::as_str)
        })
    };
    let path = call
        .locations
        .first()
        .map(|location| location.path.display().to_string())
        .or_else(|| arg(&["file_path", "path", "filePath"]).map(str::to_owned))
        .or_else(|| {
            call.content.iter().find_map(|content| match content {
                ToolCallContent::Diff(diff) => Some(diff.path.display().to_string()),
                _ => None,
            })
        });
    let target = match name {
        "bash" => arg(&["command", "cmd"])
            .map(str::to_owned)
            .or_else(|| Some(call.title.clone())),
        "read" | "edit" | "write" | "delete" | "move" => path.as_deref().map(|path| {
            let file = std::path::Path::new(path);
            model
                .session
                .as_ref()
                .and_then(|session| file.strip_prefix(&session.cwd).ok())
                .unwrap_or(file)
                .display()
                .to_string()
        }),
        "glob" | "grep" => arg(&["pattern", "glob", "query"]).map(str::to_owned),
        "find" | "web" | "fetch" => arg(&["query", "url", "pattern"]).map(str::to_owned),
        _ => None,
    }
    // Without a path or argument the agent's own title is the most specific label we have.
    .or_else(|| {
        title_for(name)
            .filter(|verb| *verb != call.title)
            .map(|_| call.title.clone())
    });
    let title = title_for(name).unwrap_or(&call.title);
    let mut node = ui::tool(name)
        .key(key)
        .role(format!("omp.tool.{name}"))
        .title(title.to_owned())
        .status(status)
        .frame(if name == "bash" {
            ToolFrame::Card
        } else {
            ToolFrame::Inline
        })
        .collapsible(name == "bash" && finished)
        .collapsed(name == "bash" && finished);
    if let Some(target) = target {
        let target_kind = match name {
            "bash" => TargetKind::Command,
            "read" | "edit" | "write" | "delete" | "move" => TargetKind::Path,
            "glob" | "grep" => TargetKind::Pattern,
            "find" | "web" | "fetch" => TargetKind::Query,
            _ => TargetKind::Text,
        };
        // A title standing in for a missing argument is prose, not a command or path.
        let from_title = if name == "bash" {
            arg(&["command", "cmd"]).is_none()
        } else {
            path.is_none() && arg(&["pattern", "glob", "query", "url"]).is_none()
        };
        let target_kind = if from_title {
            TargetKind::Text
        } else {
            target_kind
        };
        node = node.target(target).target_kind(target_kind);
    }
    if let Some(path) = path
        .as_deref()
        .filter(|_| matches!(name, "read" | "edit" | "write" | "delete" | "move"))
    {
        let full = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            model.session.as_ref().map_or_else(
                || std::path::PathBuf::from(path),
                |session| session.cwd.join(path),
            )
        };
        node = node.prop("href", format!("file://{}", full.display()));
    }
    if name == "bash" {
        node = node.prop("lang", "bash");
        if finished {
            node = node.preview(ToolPreview::Tail(10));
        }
    }
    if let Some(entry) = model.transcript.iter().find(|entry| matches!(&entry.kind, EntryKind::Tool { call: old, .. } if old.tool_call_id == call.tool_call_id)) {
        let duration = entry.finished_at.unwrap_or_else(std::time::SystemTime::now).duration_since(entry.at).unwrap_or_default().as_millis();
        let ms = u64::try_from(duration).unwrap_or(u64::MAX);
        node = if finished { node.took(ms) } else { node.age(i64::try_from(ms).unwrap_or(i64::MAX)) };
    }
    if let Some(meta) = meta {
        let mut badges = Vec::new();
        if meta.mcp {
            badges.push(ui::BadgeSpec {
                text: "MCP".into(),
                ..Default::default()
            });
        }
        if let Some(code) = meta.exit_code {
            if code != 0 {
                node = node.exit(i64::from(code));
                badges.push(ui::BadgeSpec {
                    text: format!("Exit {code}"),
                    ..Default::default()
                });
            }
        } else if let Some(signal) = &meta.signal {
            badges.push(ui::BadgeSpec {
                text: format!("Signal {signal}"),
                ..Default::default()
            });
        }
        if !badges.is_empty() {
            node = node.badges(badges);
        }
    }
    for (i, content) in call.content.iter().enumerate() {
        let body: Node<Msg> = match content {
            ToolCallContent::Content(content)
                if name == "read"
                    && matches!(
                        &content.content,
                        agent_client_protocol::schema::v1::ContentBlock::Text(_)
                    ) =>
            {
                continue;
            }
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
            ToolCallContent::Content(content)
                if name == "bash"
                    && matches!(
                        &content.content,
                        agent_client_protocol::schema::v1::ContentBlock::Text(_)
                    ) =>
            {
                let agent_client_protocol::schema::v1::ContentBlock::Text(text) = &content.content
                else {
                    unreachable!()
                };
                ui::ansi(text.text.clone())
                    .key(i)
                    .role("omp.tool.bash.output")
                    .prop("follow", false)
                    .into()
            }
            ToolCallContent::Content(content) => {
                super::content::block(&content.content, format!("{i}"), model)
            }
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
    if call.content.is_empty() {
        if name == "write"
            && let Some(source) = arg(&["content", "text"])
        {
            let mut code = ui::code(source.to_owned())
                .key("source")
                .role("omp.tool.write.source")
                .numbers(true);
            if let Some(ext) = path
                .as_deref()
                .and_then(|path| std::path::Path::new(path).extension())
            {
                code = code.lang(ext.to_string_lossy().into_owned());
            }
            node = node.child(code);
        } else if name == "edit"
            && let (Some(old), Some(new)) = (
                arg(&["old_string", "oldText"]),
                arg(&["new_string", "newText"]),
            )
        {
            node = node.child(
                ui::diff()
                    .key("patch")
                    .path(path.as_deref().unwrap_or_default())
                    .hunks(vec![Hunk {
                        old_start: Some(1),
                        new_start: Some(1),
                        lines: old
                            .lines()
                            .map(|line| format!("-{line}"))
                            .chain(new.lines().map(|line| format!("+{line}")))
                            .collect(),
                    }]),
            );
        }
    }
    if let Some(meta) = meta {
        if meta.terminal_id.is_some() {
            node = node.child(
                ui::ansi(meta.output.clone())
                    .key("terminal.output")
                    .role("omp.tool.bash.output")
                    .prop("follow", false),
            );
            if meta.truncated {
                node = node.child(
                    ui::text("Earlier output truncated")
                        .key("terminal.truncated")
                        .role("omp.tool.notice"),
                );
            }
        }
        if !meta.mcp_output.is_empty() {
            node = node.child(ui::ansi(meta.mcp_output.clone()).key("mcp.output"));
        }
        if let Some(error) = &meta.error {
            node = node.child(
                ui::text(error.clone())
                    .key("codex.error")
                    .role("omp.tool.error")
                    .tone("error"),
            );
        }
    }
    if let Some(meta) = meta
        && let Some(id) = meta
            .terminal_id
            .as_deref()
            .map(|id| agent_client_protocol::schema::v1::TerminalId::new(id.to_owned()))
        && model
            .content
            .terminals
            .get(&id)
            .is_some_and(|terminal| !terminal.released && terminal.status.is_none())
    {
        node = node.child(
            html::button("Kill terminal")
                .key("kill")
                .on_click(Msg::KillTerminal(id)),
        );
    }
    if matches!(name, "generic" | "mcp")
        && let Some(raw) = &call.raw_input
    {
        node = node.child(
            ui::section()
                .key("raw.in")
                .head("Input")
                .collapsible(true)
                .collapsed(true)
                .child(ui::code(raw.to_string()).key("body")),
        );
    }
    if matches!(name, "generic" | "mcp")
        && let Some(raw) = &call.raw_output
    {
        node = node.child(
            ui::section()
                .key("raw.out")
                .head("Output")
                .collapsible(true)
                .collapsed(true)
                .child(ui::code(raw.to_string()).key("body")),
        );
    }
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
