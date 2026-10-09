//! Whole-view rendering from session state; no terminal I/O.

use agent_client_protocol::schema::v1::{
    ContentBlock, PlanEntryStatus, SessionConfigOptionCategory, StopReason, ToolCallStatus,
};
use chrono::{DateTime, Local};
use tern_sdk::{
    Capabilities,
    ui::{
        self, Align, ChecklistItem, ChecklistMode, Gap, ItemStatus, MeterStyle, Node, Phase,
        ShimmerMode, ShimmerPalette, SpinnerStyle, Thresholds, Tone, View,
    },
};

use super::ext;
use super::{Msg, auth, composer::Composer, palette, prefs};
use crate::model::{EntryKind, Model, Turn, TurnSummary};

/// The composer editor's node id (`dock` > `editor` > `line` > `ed`). Native edit events,
/// focus and caret-anchored popups all name it; `editor_id_matches_the_rendered_composer`
/// keeps it in sync with `composer_dock`.
pub const EDITOR_ID: &str = "dock.editor.line.ed";

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
    caps: &Capabilities,
    form: &super::elicit::FormState,
    picker: Option<&super::attach::AttachPicker>,
) -> View<Msg> {
    let mut main = ui::col().key("transcript");
    // Tern's omp reader starts with a small gap and keeps the hero card visible.
    main = main.child(ui::spacer().key("welcome.before").size(Gap::Sm));
    main = main.child(welcome(model));
    main = main.child(ui::spacer().key("welcome.after").size(Gap::Sm));
    for entry in &model.transcript {
        let key = format!("entry.{}", entry.id);
        let scope = key.clone();
        let node: Node<Msg> = match &entry.kind {
            EntryKind::User { blocks, .. } => ui::card()
                .key(key)
                .role("omp.user")
                .tone("user")
                .actions(ui::Actions::new().menu("copy").menu("edit"))
                .on_menu("edit", Msg::EditPrompt(entry.id))
                .child(
                    ui::row()
                        .key("tools")
                        .gap(Gap::Xs)
                        .role("omp.user.tools")
                        .child(
                            ui::text(
                                ui::span(
                                    DateTime::<Local>::from(entry.at)
                                        .format("%H:%M")
                                        .to_string(),
                                )
                                .style("dim mono"),
                            )
                            .key("time")
                            .role("omp.user.time")
                            .title(
                                DateTime::<Local>::from(entry.at)
                                    .format("%Y-%m-%d %H:%M:%S %Z")
                                    .to_string(),
                            ),
                        ),
                )
                .children(super::content::user_blocks(
                    blocks, model, caps, &scope, entry.id,
                ))
                .into(),
            EntryKind::Steering { blocks } => ui::card()
                .key(key)
                .role("omp.user.synthetic")
                .tone("muted")
                .head("Steering")
                .children(super::content::blocks(blocks, "body", model, caps, &scope))
                .into(),
            EntryKind::Agent { blocks, .. } => {
                let mut col = ui::col().key(key).role("omp.assistant");
                for (i, block) in blocks.iter().enumerate() {
                    let body: Node<Msg> = match block {
                        ContentBlock::Text(text) => {
                            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text.text)
                                && (value.is_object() || value.is_array())
                                && (super::supports(caps, "tree") || super::supports(caps, "table"))
                            {
                                super::content::json_display(
                                    &value,
                                    &text.text,
                                    &format!("body.{i}"),
                                    model,
                                    caps,
                                    &scope,
                                )
                            } else {
                                ui::md(super::content::line_breaks(&text.text))
                                    .key(format!("body.{i}"))
                                    .stream(model.is_streaming(entry))
                                    .actions(super::content::copy_menu())
                                    .into()
                            }
                        }
                        _ => super::content::block(block, format!("body.{i}"), model, caps, &scope),
                    };
                    col = col.child(body);
                }
                if model
                    .transcript
                    .iter()
                    .skip_while(|other| other.id != entry.id)
                    .skip(1)
                    .filter(|other| other.turn_id() == entry.turn_id())
                    .all(|other| !matches!(other.kind, EntryKind::Agent { .. }))
                    && let Some(summary) = model.turn_summaries.get(&entry.turn_id())
                {
                    col = col.child(turn_usage(summary));
                }
                col.into()
            }
            EntryKind::Thought { blocks, .. } => {
                let live = model.is_streaming(entry) && entry.finished_at.is_none();
                let seconds = entry
                    .finished_at
                    .unwrap_or_else(std::time::SystemTime::now)
                    .duration_since(entry.at)
                    .unwrap_or_default()
                    .as_secs();
                let head: Node<Msg> = if live {
                    ui::row()
                        .key("head")
                        .gap(Gap::Sm)
                        .child(
                            ui::spinner()
                                .key("spin")
                                .role("omp.thinking.spin")
                                .style(SpinnerStyle::Starburst),
                        )
                        .child(ui::text(ui::span("Thinking…").style("muted")).key("label"))
                        .child(ui::elapsed().key("elapsed").prop(
                            "age",
                            i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX),
                        ))
                        .into()
                } else {
                    ui::text(ui::span(format!("Thought for {seconds}s")).style("muted"))
                        .key("head")
                        .into()
                };
                let fold_key = scope.clone();
                ui::section()
                    .key(key)
                    .role(if live {
                        "omp.thinking.live"
                    } else {
                        "omp.thinking"
                    })
                    .collapsible(true)
                    .collapsed(model.folds.get(&fold_key).copied().unwrap_or(!live))
                    .on_toggle(move |collapsed| Msg::ToggleFold(fold_key.clone(), collapsed))
                    .prop("took", seconds.saturating_mul(1000))
                    .child(head)
                    .children(super::content::blocks(blocks, "body", model, caps, &scope))
                    .into()
            }
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
                ext::tool(call, *cancelled, key, model, 0, caps)
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
                    .role("omp.hud.todo")
                    .mode(ChecklistMode::Hud)
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
                        .role("omp.hud.todo")
                        .mode(ChecklistMode::Hud)
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
                let fold_key = scope.clone();
                let mut section = ui::section()
                    .role("omp.compaction")
                    .key(key)
                    .head("Context compacted")
                    .collapsible(true)
                    .collapsed(model.folds.get(&fold_key).copied().unwrap_or(false))
                    .on_toggle(move |collapsed| Msg::ToggleFold(fold_key.clone(), collapsed));
                if matches!(
                    status,
                    agent_client_protocol::schema::v1::CompactionStatus::InProgress
                ) {
                    section = section.child(ui::spinner().key("progress").label("Compacting"));
                }
                for block in super::content::blocks(summary, "summary", model, caps, &scope) {
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
                let fold_key = scope.clone();
                let mut agent = ui::agent(title.clone())
                    .key(key)
                    .depth(1_u32)
                    .collapsible(true)
                    .collapsed(model.folds.get(&fold_key).copied().unwrap_or(false))
                    .on_toggle(move |collapsed| Msg::ToggleFold(fold_key.clone(), collapsed))
                    .status(status);
                if let Some(description) = description {
                    agent = agent.child(ui::text(description.clone()).key("description"));
                }
                for (index, message) in messages.iter().enumerate() {
                    if let agent_client_protocol::schema::MaybeUndefined::Value(blocks) =
                        &message.content
                    {
                        for block in super::content::blocks(
                            blocks,
                            &format!("message.{index}"),
                            model,
                            caps,
                            &scope,
                        ) {
                            agent = agent.child(block);
                        }
                    }
                }
                agent.into()
            }
            #[cfg(feature = "unstable_end_turn_token_usage")]
            EntryKind::Usage(_) => {
                if model.transcript.iter().any(|other| {
                    other.turn_id() == entry.turn_id()
                        && matches!(other.kind, EntryKind::Agent { .. })
                }) {
                    continue;
                }
                if let Some(summary) = model.turn_summaries.get(&entry.turn_id()) {
                    turn_usage(summary).key(key).into()
                } else {
                    continue;
                }
            }
        };
        main = main.child(node);
    }
    for (i, error) in model.errors.iter().enumerate() {
        main = main.child(
            ui::card()
                .role("omp.error")
                .key(format!("error.{i}"))
                .tone("error")
                .child(ui::md(error.clone()).key("body")),
        );
    }
    for (i, notice) in model.notices.iter().enumerate() {
        main = main.child(
            ui::card()
                .key(format!("notice.{i}"))
                .role("omp.hint")
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
            .role("omp.notice")
            .key(format!("agent.notice.{i}"))
            .tone(tone);
        if let Some(description) = &notice.description {
            toast = toast.sub(description.clone());
        }
        main = main.child(toast);
    }

    if let Some(rate) = &model.ext.rate_limit {
        main = main.child(
            ui::card()
                .key("rate.limit")
                .role("omp.notice.warning")
                .tone("warning")
                .child(ui::text(rate.clone()).key("message")),
        );
    }
    if let Some(toast) = &model.ext.toast {
        main = main.child(
            ui::toast(toast.clone())
                .key("extension.result")
                .role("omp.notice"),
        );
    }
    let dock = composer_dock(model, composer);
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
        let mut actions = ui::row()
            .key("actions")
            .role("omp.error.actions")
            .gap(Gap::Sm);
        if model.restartable {
            actions = actions.child(
                ui::row()
                    .key("restart")
                    .role("omp.error.action")
                    .gap(Gap::Xs)
                    .align(Align::Center)
                    .on_click(Msg::Restart)
                    .child(ui::text("Restart").key("label"))
                    .child(ui::kbd(["r"]).key("key")),
            );
        }
        actions = actions.child(
            ui::row()
                .key("quit")
                .role("omp.error.action")
                .gap(Gap::Xs)
                .align(Align::Center)
                .on_click(Msg::Quit)
                .child(ui::text("Quit").key("label"))
                .child(ui::kbd(["ctrl", "d"]).key("key")),
        );
        result = result.layer(
            ui::col().child(
                ui::overlay()
                    .modal(true)
                    .key("exited")
                    .role("omp.overlay.error")
                    .head("Agent exited")
                    .child(
                        ui::md(reason.clone())
                            .key("reason")
                            .role("omp.error.message"),
                    )
                    .child(actions),
            ),
        );
    }

    result
}
/// The hero uses omp's lockup shape, but never its logo or tip copy.
fn welcome(model: &Model) -> Node<Msg> {
    let title = model
        .agent
        .as_ref()
        .map(|agent| agent.title.as_deref().unwrap_or(&agent.name))
        .unwrap_or(&model.launch_name);
    let version = model.agent.as_ref().map_or_else(
        || "Connecting…".to_owned(),
        |agent| format!("v{}", agent.version.trim_start_matches('v')),
    );
    let cwd = model
        .session
        .as_ref()
        .map(|session| session.cwd.display().to_string())
        .unwrap_or_else(|| {
            if model.status.is_empty() {
                "Starting session…".into()
            } else {
                model.status.clone()
            }
        });
    let mut lockup = ui::row()
        .key("lockup")
        .role("omp.welcome.lockup")
        .align(Align::Center)
        .gap(Gap::Md);
    // These marks are supplied by Tern itself, not bundled or downloaded by the adapter.
    let icon = if model.launch_name.eq_ignore_ascii_case("codex") {
        Some("app-codex")
    } else if model.launch_name.eq_ignore_ascii_case("claude") {
        Some("app-claude")
    } else {
        None
    };
    let mark: Node<Msg> = if let Some(icon) = icon {
        ui::icon(icon)
            .key("glyph")
            .role("omp.welcome.logo")
            .tone("accent")
            .title(title)
            .into()
    } else {
        ui::text("✦")
            .key("glyph")
            .role("omp.welcome.logo")
            .tone("accent")
            .title(title)
            .into()
    };
    lockup = lockup.child(mark);
    ui::card()
        .key("welcome")
        .role("omp.welcome")
        .child(
            lockup.child(
                ui::col()
                    .key("mark")
                    .role("omp.welcome.mark")
                    .child(
                        ui::text(ui::span(title).style("strong"))
                            .key("wordmark")
                            .role("omp.welcome.wordmark"),
                    )
                    .child(
                        ui::text(ui::span(version).style("dim mono"))
                            .key("version")
                            .role("omp.welcome.version"),
                    ),
            ),
        )
        .child(
            ui::row()
                .key("tip")
                .role("omp.welcome.tip")
                .gap(Gap::Sm)
                .align(Align::Start)
                .child(ui::icon("folder").key("icon").role("omp.welcome.tip-icon"))
                .child(
                    ui::text(format!("{cwd}  ·  / commands  ·  @ files"))
                        .key("text")
                        .role("omp.welcome.tip-text"),
                ),
        )
        .into()
}

fn turn_usage(summary: &TurnSummary) -> ui::Row<Msg> {
    let seconds = summary.duration.as_secs_f64();
    let stopped = summary.stop.map(|reason| match reason {
        StopReason::Cancelled => "Stopped",
        StopReason::MaxTokens => "Hit the token limit",
        StopReason::MaxTurnRequests => "Hit the request limit",
        StopReason::Refusal => "Refused",
        _ => "Ended early",
    });
    let mut text = stopped.map_or_else(String::new, |label| format!("{label} · "));
    text.push_str(&format!("{seconds:.1}s"));
    let mut detail = vec![format!("This turn: {seconds:.1}s")];
    #[cfg(feature = "unstable_end_turn_token_usage")]
    if let Some(usage) = &summary.tokens {
        let tokens = TurnTokens::from(usage);
        if tokens.input > 0 {
            text.push_str(&format!(" · {} in", compact_tokens(tokens.input)));
            if tokens.cached_read > 0 {
                let share = tokens.cached_read as f64 / tokens.input as f64 * 100.0;
                text.push_str(&format!(" ({share:.0}% cached)"));
            }
            detail.push(format!(
                "Input: {} tokens ({} new · {} cache read · {} cache write)",
                grouped(tokens.input),
                grouped(usage.input_tokens),
                grouped(tokens.cached_read),
                grouped(tokens.cached_write)
            ));
        }
        if tokens.output > 0 {
            text.push_str(&format!(" · {} out", compact_tokens(tokens.output)));
            detail.push(format!("Output: {} tokens", grouped(tokens.output)));
        }
        if tokens.thinking > 0 {
            text.push_str(&format!(" · {} thinking", compact_tokens(tokens.thinking)));
            detail.push(format!("Thinking: {} tokens", grouped(tokens.thinking)));
        }
    }
    // Sub-cent costs read as noise ("$0.00"); the tooltip keeps the exact amount.
    if let Some((currency, cost)) = &summary.cost {
        let symbol = if currency == "USD" {
            "$"
        } else {
            currency.as_str()
        };
        if *cost >= 0.005 {
            text.push_str(&format!(" · {symbol}{cost:.2}"));
        }
        detail.push(format!("Cost: {symbol}{cost:.4}"));
    }
    let detail = detail.join("\n");
    let mut row = ui::row();
    if stopped.is_some() {
        row = row.tone(Tone::Warning);
    }
    row.key("turn-usage")
        .role("omp.turn.usage")
        .gap(Gap::Xs)
        .align(Align::Center)
        .title(detail)
        .child(ui::icon("time").key("icon"))
        .child(ui::text(text).key("text"))
}

/// Dock chrome follows the `omp.editor` layout; Tern's bundled CSS owns its appearance.
fn composer_dock(model: &Model, composer: &Composer) -> Node<Msg> {
    let title = model
        .agent
        .as_ref()
        .map(|agent| agent.title.as_deref().unwrap_or(&agent.name))
        .unwrap_or(&model.launch_name);
    let running = matches!(model.turn, Turn::Running { .. });
    let blocked = model.dialogs.front().is_some() || model.auth.is_some() || model.exited.is_some();
    let mut dock = ui::col();
    if let Turn::Running { since, cancelling } = model.turn {
        let label = model
            .transcript
            .iter()
            .rev()
            .find(|entry| model.is_streaming(entry))
            .map(|entry| match &entry.kind {
                EntryKind::Tool { call, .. }
                    if matches!(
                        call.status,
                        ToolCallStatus::Pending | ToolCallStatus::InProgress
                    ) =>
                {
                    format!("{}…", call.title)
                }
                EntryKind::Thought { .. } => "Thinking…".into(),
                _ => "Working…".into(),
            })
            .unwrap_or_else(|| "Working…".into());
        let row = ui::row().key("activity").role("omp.hud.activity").child(
            ui::col().key("status").role("omp.hud.status").child(
                ui::row()
                    .key("working")
                    .role("omp.working")
                    .align(Align::Center)
                    .gap(Gap::Sm)
                    .child(
                        ui::spinner()
                            .key("spinner")
                            .style(SpinnerStyle::Starburst)
                            .tone("accent"),
                    )
                    .child(
                        ui::elapsed()
                            .key("elapsed")
                            .since(since)
                            .format(ui::ElapsedFormat::Short),
                    )
                    .child(ui::text("·").key("sep").role("omp.working.sep"))
                    .child(
                        ui::shimmer(if cancelling { "Cancelling…" } else { &label })
                            .key("label")
                            .role("omp.working.label")
                            .mode(ShimmerMode::Classic)
                            .palette(ShimmerPalette {
                                low: Some("dim".into()),
                                mid: Some("muted".into()),
                                high: Some("accent".into()),
                            }),
                    )
                    .child(ui::row().key("fill").prop("grow", 1))
                    .child(
                        ui::row()
                            .key("stop")
                            .role("omp.working.stop")
                            .align(Align::Center)
                            .gap(Gap::Xs)
                            .title("Stop  Esc")
                            .on_click(Msg::Interrupt)
                            .child(ui::kbd(["escape"]).key("key"))
                            .child(ui::text("Stop").key("text")),
                    ),
            ),
        );
        dock = dock.child(row);
    }
    let mut editor = ui::col().key("editor").role("omp.editor");
    if running {
        editor = editor.tone("pending");
    }
    if let Some(usage) = &model.content.usage {
        let ratio = if usage.size == 0 {
            0.0
        } else {
            (usage.used as f64 / usage.size as f64).clamp(0.0, 1.0)
        };
        editor = editor.child(
            ui::meter()
                .key("context")
                .role("omp.composer.context")
                .style(MeterStyle::Bar)
                .thresholds(Thresholds {
                    warn: Some(0.15),
                    bad: Some(0.5),
                })
                .value(ratio)
                .label(format!("{:.0}%", ratio * 100.0))
                .total(compact_tokens(usage.size))
                .title(format!(
                    "Context: {} of {} tokens ({:.0}%)",
                    usage.used,
                    usage.size,
                    ratio * 100.0
                ))
                .on_click(Msg::UsageOpen),
        );
    }
    editor = editor.child(
        ui::row()
            .key("line")
            .role("omp.composer.line")
            .align(Align::Start)
            .gap(Gap::Sm)
            .child(
                ui::editor()
                    .key("ed")
                    .text(composer.text.clone())
                    .cursor(composer.utf16_cursor())
                    .placeholder(format!("Message {title}…"))
                    .max_lines(18_u32)
                    .readonly(blocked)
                    .sendable(!blocked)
                    .on_send(|text| Msg::Send(text.to_owned()))
                    .on_edit(|edit| Msg::Edit(edit.clone()))
                    .on_undo(Msg::Undo)
                    .on_focus(Msg::Focus),
            ),
    );
    if !composer.images.is_empty() {
        editor = editor.child(
            ui::row()
                .key("attachments")
                .role("omp.composer.attachments")
                .gap(Gap::Sm)
                .children(composer.images.iter().enumerate().map(|(index, image)| {
                    let label = image
                        .path
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .map_or_else(
                            || "Clipboard image".to_owned(),
                            |name| name.to_string_lossy().into_owned(),
                        );
                    let thumbnail: Node<Msg> = if let Some(blob) = &image.blob {
                        ui::image()
                            .blob(blob.clone())
                            .alt(label.clone())
                            .w(26.0)
                            .h(26.0)
                            .key("icon")
                            .into()
                    } else {
                        ui::icon("image").key("icon").into()
                    };
                    ui::row()
                        .key(format!("image.{index}"))
                        .role("omp.composer.attachment")
                        .gap(Gap::Xs)
                        .align(Align::Center)
                        .child(thumbnail)
                        .child(ui::text(label).key("name"))
                        .child(
                            ui::icon("x")
                                .key("remove")
                                .title("Remove image")
                                .on_click(Msg::ImageRemove(index)),
                        )
                })),
        );
    }
    let model_label = prefs::chip(model, SessionConfigOptionCategory::Model)
        .and_then(|label| label.split_once(": ").map(|(_, value)| value.to_owned()))
        .unwrap_or_else(|| title.to_owned());
    let mut model_chip = ui::row()
        .key("model")
        .role("omp.composer.model")
        .gap(Gap::Xs)
        .align(Align::Center)
        .title("Switch model")
        .on_click(Msg::ModelPicker);
    if palette::has_category(model, SessionConfigOptionCategory::ThoughtLevel) {
        if let Some(level) = prefs::chip(model, SessionConfigOptionCategory::ThoughtLevel) {
            let level = level
                .split_once(": ")
                .map_or(level.as_str(), |(_, value)| value);
            model_chip = model_chip.child(
                ui::node("effort")
                    .key("effort")
                    .role("omp.composer.model.effort")
                    .prop("level", level.to_lowercase())
                    .title(format!("Thinking effort: {level}"))
                    .on_click(Msg::EffortPicker),
            );
        }
    } else {
        model_chip = model_chip.child(ui::icon("model").key("icon"));
    }
    model_chip = model_chip
        .child(ui::text(ui::span(model_label).style("statusLineModel")).key("name"))
        .child(ui::icon("chev").key("chev"));
    let mut extras = ui::status()
        .key("extras")
        .role("omp.composer.extras")
        .transparent(true)
        .prop("grow", 1);
    let mode = if palette::has_category(model, SessionConfigOptionCategory::Mode) {
        prefs::chip(model, SessionConfigOptionCategory::Mode)
    } else {
        model
            .session
            .as_ref()
            .and_then(|session| session.modes.as_ref())
            .and_then(|modes| {
                modes
                    .available_modes
                    .iter()
                    .find(|item| item.id == modes.current_mode_id)
                    .map(|item| item.name.clone())
            })
    };
    if let Some(mode) = mode {
        extras = extras.child(
            ui::seg(mode)
                .key("mode")
                .role("omp.composer.mode")
                .priority(6.0)
                .icon("mode")
                .title("Choose mode")
                .on_click(Msg::ModePicker),
        );
    }
    for (id, label, _) in prefs::active_chips(model) {
        extras = extras.child(
            ui::seg(label)
                .key(format!("config.{id}"))
                .role("omp.composer.fact")
                .priority(6.0)
                .icon("mode")
                .title("Return to default setting")
                .on_click(Msg::ConfigChip(id)),
        );
    }
    // No directory chip: Tern adds the pane's place as a crumb in omp's composer bar, as for
    // omp; a chip of our own showed the folder twice.
    if let Some(name) = model
        .session
        .as_ref()
        .and_then(|session| session.title.as_ref())
    {
        extras = extras.child(
            ui::seg(name.clone())
                .key("session")
                .role("omp.composer.fact")
                .priority(2.0),
        );
    }
    if let Some(auth) = &model.ext.auth
        && let Some(label) = &auth.label
    {
        extras = extras.child(
            ui::seg(label.clone())
                .key("account")
                .role("omp.composer.fact")
                .priority(3.0)
                .on_click(Msg::AccountDetails),
        );
    }
    if model.queued > 0 {
        extras = extras.child(
            ui::seg(format!("{} queued", model.queued))
                .key("queue")
                .role("omp.composer.fact")
                .priority(5.0),
        );
    }
    let mut bar = ui::row()
        .key("bar")
        .role("omp.composer.bar")
        .gap(Gap::Sm)
        .align(Align::Center)
        .child(model_chip)
        .child(extras);
    let cost = super::usage::session_cost(model);
    let label = if let Some((currency, amount)) = &cost {
        format!(
            "{}{amount:.2}",
            if currency == "USD" { "$" } else { currency }
        )
    } else {
        let count =
            model.turn_summaries.len() + usize::from(running && !model.usage_report.fetching);
        format!("{count} turn{}", if count == 1 { "" } else { "s" })
    };
    bar = bar.child(
        ui::text(label)
            .key("usage")
            .role("omp.composer.usage")
            .title("Usage report")
            .on_click(Msg::UsageOpen),
    );
    bar = if running {
        bar.child(
            ui::text("Stop")
                .key("send")
                .role("omp.composer.stop")
                .tone("error")
                .title("Stop  Esc")
                .on_click(Msg::Interrupt),
        )
    } else {
        bar.child(
            ui::kbd(["enter"])
                .key("send")
                .role("omp.composer.send")
                .title("Send  Enter")
                .on_click(Msg::Submit),
        )
    };
    dock.child(editor.child(bar)).into()
}

#[cfg(feature = "unstable_end_turn_token_usage")]
/// One turn's token usage, normalised across adapters: ACP's `inputTokens` is the fresh input
/// only, and cache reads/writes come on top (Claude reads most of its context from cache, so
/// `inputTokens` alone would say "4 in").
struct TurnTokens {
    input: u64,
    cached_read: u64,
    cached_write: u64,
    output: u64,
    thinking: u64,
}

#[cfg(feature = "unstable_end_turn_token_usage")]
impl From<&agent_client_protocol::schema::v1::Usage> for TurnTokens {
    fn from(usage: &agent_client_protocol::schema::v1::Usage) -> Self {
        let cached_read = usage.cached_read_tokens.unwrap_or(0);
        let cached_write = usage.cached_write_tokens.unwrap_or(0);
        Self {
            input: usage.input_tokens + cached_read + cached_write,
            cached_read,
            cached_write,
            output: usage.output_tokens,
            thinking: usage.thought_tokens.unwrap_or(0),
        }
    }
}

/// `16913` → `16,913`.
#[cfg(feature = "unstable_end_turn_token_usage")]
fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Token counts the way omp's context meter shows them: `1M`, `258K`, `950`.
pub(super) fn compact_tokens(count: u64) -> String {
    match count {
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 10_000 => format!("{}K", (n + 500) / 1_000),
        n if n >= 1_000 => format!("{:.1}K", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "unstable_end_turn_token_usage")]
    #[test]
    fn turn_footer_counts_cached_context_as_input() {
        use agent_client_protocol::schema::v1::Usage;
        let footer = |usage: Usage, cost: Option<f64>| {
            let summary = TurnSummary {
                duration: std::time::Duration::from_millis(6_800),
                tokens: Some(usage),
                cost: cost.map(|cost| ("USD".to_owned(), cost)),
                stop: None,
            };
            serde_json::to_string(
                &tern_sdk::reconcile::Doc::from_view(
                    View::<Msg>::new().main(ui::col().child(turn_usage(&summary))),
                )
                .unwrap()
                .regions()[0]
                    .as_ref()
                    .unwrap()
                    .wire(),
            )
            .unwrap()
        };
        // Codex: 4,619 fresh + 12,288 cached input, 6 out, no cost (subscription).
        let mut codex = Usage::new(16_913, 4_619, 6);
        codex.cached_read_tokens = Some(12_288);
        let codex = footer(codex, None);
        assert!(
            codex.contains("6.8s · 17K in (73% cached) · 6 out"),
            "{codex}"
        );
        // Claude: ACP inputTokens is only the uncached tail (4); the context came from cache.
        let mut claude = Usage::new(46_917, 4, 105);
        claude.cached_read_tokens = Some(37_112);
        claude.cached_write_tokens = Some(9_696);
        let claude = footer(claude, Some(0.0881));
        assert!(
            claude.contains("47K in (79% cached) · 105 out · $0.09"),
            "{claude}"
        );
        // Sub-cent cost and empty usage stay out of the line (a turn stopped before output).
        let empty = footer(Usage::new(0, 0, 0), Some(0.001));
        assert!(empty.contains("\"text\":\"6.8s\""), "{empty}");
    }

    #[test]
    fn context_totals_read_like_omp() {
        assert_eq!(compact_tokens(1_000_000), "1M");
        assert_eq!(compact_tokens(1_500_000), "1.5M");
        assert_eq!(compact_tokens(258_400), "258K");
        assert_eq!(compact_tokens(950), "950");
    }

    #[test]
    fn editor_id_matches_the_rendered_composer() {
        let view = view(
            &Model::default(),
            &Composer::default(),
            &Capabilities::default(),
        );
        let doc = tern_sdk::reconcile::Doc::from_view(view).unwrap();
        let dock = serde_json::to_string(&doc.regions()[1].as_ref().unwrap().wire()).unwrap();
        assert!(
            dock.contains(&format!("\"id\":\"{EDITOR_ID}\",\"k\":\"editor\"")),
            "{dock}"
        );
    }
    #[test]
    fn dashboard_and_plan_chip_render_with_omp_roles() {
        use agent_client_protocol::schema::v1::{
            SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption, SessionId,
        };
        let mut model = Model::default();
        model.launch_name = "Codex".into();
        model.apply(crate::event::BridgeEvent::SessionStarted {
            session_id: SessionId::new("session"),
            kind: crate::event::SessionStartKind::New,
            cwd: std::path::PathBuf::from("/tmp"),
            additional_dirs: vec![],
            modes: None,
            config_options: Some(vec![
                SessionConfigOption::select(
                    "collaboration_mode",
                    "Collaboration mode",
                    "plan",
                    vec![
                        SessionConfigSelectOption::new("default", "Default"),
                        SessionConfigSelectOption::new("plan", "Plan"),
                    ],
                )
                .category(SessionConfigOptionCategory::Other(
                    "collaboration_mode".into(),
                )),
            ]),
        });
        let dock = serde_json::to_string(
            &tern_sdk::reconcile::Doc::from_view(view(
                &model,
                &Composer::default(),
                &Capabilities::default(),
            ))
            .unwrap()
            .regions()[1]
                .as_ref()
                .unwrap()
                .wire(),
        )
        .unwrap();
        assert!(dock.contains("omp.composer.usage"), "{dock}");
        assert!(dock.contains("config.collaboration_mode"), "{dock}");
        assert!(dock.contains("Plan"), "{dock}");
        model.usage_report.open = true;
        model.usage_report.raw = "**5h limit:** 92% left (resets 14:00)".into();
        model
            .usage_report
            .windows
            .push(crate::model::usage::LimitWindow {
                label: "5h limit".into(),
                used_pct: 8.0,
                resets: "resets 14:00".into(),
            });
        let overlay = serde_json::to_string(
            &tern_sdk::reconcile::Doc::from_view(
                view(&model, &Composer::default(), &Capabilities::default()).layer(
                    super::super::usage::render(&model, &Capabilities::default()),
                ),
            )
            .unwrap()
            .regions()[2]
                .as_ref()
                .unwrap()
                .wire(),
        )
        .unwrap();
        assert!(overlay.contains("omp.usage.window"), "{overlay}");
        assert!(overlay.contains("92% left"), "{overlay}");
        assert!(overlay.contains("This session"), "{overlay}");
        model.usage_report.details = true;
        let details = serde_json::to_string(
            &tern_sdk::reconcile::Doc::from_view(
                view(&model, &Composer::default(), &Capabilities::default()).layer(
                    super::super::usage::render(&model, &Capabilities::default()),
                ),
            )
            .unwrap()
            .regions()[2]
                .as_ref()
                .unwrap()
                .wire(),
        )
        .unwrap();
        assert!(details.contains("**5h limit:**"), "{details}");
    }

    #[test]
    fn composer_controls_follow_confirmed_session_options() {
        use agent_client_protocol::schema::v1::{
            SessionConfigOption, SessionConfigSelectOption, SessionId,
        };
        fn find_role<'a>(node: &'a serde_json::Value, role: &str) -> Option<&'a serde_json::Value> {
            if node["p"]["role"] == role {
                return Some(node);
            }
            node["c"]
                .as_array()?
                .iter()
                .find_map(|child| find_role(child, role))
        }
        let dock = |model: &Model| {
            serde_json::to_value(
                tern_sdk::reconcile::Doc::from_view(view(
                    model,
                    &Composer::default(),
                    &Capabilities::default(),
                ))
                .unwrap()
                .regions()[1]
                    .as_ref()
                    .unwrap()
                    .wire(),
            )
            .unwrap()
        };
        let mut model = Model::default();
        let idle = dock(&model);
        assert!(find_role(&idle, "omp.composer.model.effort").is_none());
        assert!(find_role(&idle, "omp.composer.mode").is_none());
        let start = |model: &mut Model, id: &str, options| {
            model.apply(crate::event::BridgeEvent::SessionStarted {
                session_id: SessionId::new(id),
                kind: crate::event::SessionStartKind::New,
                cwd: std::path::PathBuf::from("/tmp"),
                additional_dirs: vec![],
                modes: None,
                config_options: Some(options),
            });
        };
        start(
            &mut model,
            "one",
            vec![
                SessionConfigOption::select(
                    "reasoning_effort",
                    "Reasoning effort",
                    "medium",
                    vec![
                        SessionConfigSelectOption::new("low", "Low"),
                        SessionConfigSelectOption::new("medium", "Medium"),
                    ],
                )
                .category(SessionConfigOptionCategory::ThoughtLevel),
                SessionConfigOption::select(
                    "mode",
                    "Mode",
                    "auto",
                    vec![
                        SessionConfigSelectOption::new("read-only", "Read-only"),
                        SessionConfigSelectOption::new("auto", "Auto review"),
                    ],
                )
                .category(SessionConfigOptionCategory::Mode),
            ],
        );
        let active = dock(&model);
        assert_eq!(
            find_role(&active, "omp.composer.model.effort").unwrap()["p"]["level"],
            "medium"
        );
        let mode = find_role(&active, "omp.composer.mode").unwrap();
        assert_eq!(mode["p"]["text"], "Mode: Auto review");
        assert_eq!(mode["p"]["actions"]["click"], "click");
        start(&mut model, "two", vec![]);
        let switched = dock(&model);
        assert!(find_role(&switched, "omp.composer.model.effort").is_none());
        assert!(find_role(&switched, "omp.composer.mode").is_none());
    }
}
