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
use super::{Msg, auth, composer::Composer, prefs};
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
    _caps: &Capabilities,
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
        let node: Node<Msg> = match &entry.kind {
            EntryKind::User { blocks, .. } => ui::card()
                .key(key)
                .role("omp.user")
                .tone("user")
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
                .children(super::content::user_blocks(blocks, model))
                .into(),
            EntryKind::Steering { blocks } => ui::card()
                .key(key)
                .role("omp.user.synthetic")
                .tone("muted")
                .head("Steering")
                .children(super::content::blocks(blocks, "body", model))
                .into(),
            EntryKind::Agent { blocks, .. } => {
                let mut col = ui::col().key(key).role("omp.assistant");
                for (i, block) in blocks.iter().enumerate() {
                    let body: Node<Msg> = match block {
                        ContentBlock::Text(text) => ui::md(text.text.clone())
                            .key(format!("body.{i}"))
                            .stream(model.is_streaming(entry))
                            .into(),
                        _ => super::content::block(block, format!("body.{i}"), model),
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
                ui::section()
                    .key(key)
                    .role(if live {
                        "omp.thinking.live"
                    } else {
                        "omp.thinking"
                    })
                    .collapsible(true)
                    .collapsed(!live)
                    .prop("took", seconds.saturating_mul(1000))
                    .child(head)
                    .children(super::content::blocks(blocks, "body", model))
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
                let mut section = ui::section()
                    .role("omp.compaction")
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
        .unwrap_or_else(|| "Starting session…".into());
    ui::card()
        .key("welcome")
        .role("omp.welcome")
        .child(
            ui::row()
                .key("lockup")
                .role("omp.welcome.lockup")
                .align(Align::Center)
                .gap(Gap::Md)
                .child(
                    ui::text("✦")
                        .key("glyph")
                        .role("omp.welcome.logo")
                        .tone("accent")
                        .title(title),
                )
                .child(
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
    let mut detail = format!("This turn: {seconds:.1}s");
    if let Some((total, input, output)) = summary.tokens {
        text.push_str(&format!(" · {total} tok"));
        detail.push_str(&format!(" · {total} tokens"));
        if input > 0 || output > 0 {
            detail.push_str(&format!(" ({input} in · {output} out)"));
        }
    }
    if let Some((currency, cost)) = &summary.cost {
        let symbol = if currency == "USD" {
            "$"
        } else {
            currency.as_str()
        };
        text.push_str(&format!(" · {symbol}{cost:.2}"));
        detail.push_str(&format!(" · {symbol}{cost:.2}"));
    }
    text.push_str(" this turn");
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
                .on_click(Msg::ContextDetails),
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
    let model_label = prefs::chip(model, SessionConfigOptionCategory::Model)
        .and_then(|label| label.split_once(": ").map(|(_, value)| value.to_owned()))
        .unwrap_or_else(|| title.to_owned());
    let mut model_chip = ui::row()
        .key("model")
        .role("omp.composer.model")
        .gap(Gap::Xs)
        .align(Align::Center)
        .title("Switch model")
        .on_click(Msg::ModelPicker)
        .child(ui::icon("model").key("icon"))
        .child(ui::text(ui::span(model_label).style("statusLineModel")).key("name"));
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
    model_chip = model_chip.child(ui::icon("chev").key("chev"));
    let mut extras = ui::status()
        .key("extras")
        .role("omp.composer.extras")
        .transparent(true)
        .prop("grow", 1);
    if let Some(mode) = prefs::chip(model, SessionConfigOptionCategory::Mode).or_else(|| {
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
    }) {
        extras = extras.child(
            ui::seg(mode)
                .key("mode")
                .role("omp.composer.fact")
                .priority(6.0)
                .icon("mode"),
        );
    }
    if let Some(session) = &model.session {
        let folder = session.cwd.file_name().map_or_else(
            || session.cwd.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        extras = extras.child(
            ui::seg(ui::span(folder).style("statusLinePath"))
                .key("cwd")
                .role("omp.composer.fact")
                .priority(7.0)
                .icon("folder")
                .title(session.cwd.display().to_string()),
        );
        if let Some(name) = &session.title {
            extras = extras.child(
                ui::seg(name.clone())
                    .key("session")
                    .role("omp.composer.fact")
                    .priority(2.0),
            );
        }
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
    if let Some(cost) = model
        .content
        .usage
        .as_ref()
        .and_then(|usage| usage.cost.as_ref())
    {
        let symbol = if cost.currency == "USD" {
            "$"
        } else {
            cost.currency.as_str()
        };
        bar = bar.child(
            ui::text(format!("{symbol}{:.2}", cost.amount))
                .key("usage")
                .role("omp.composer.usage")
                .title("Session cost"),
        );
    }
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

/// Token counts the way omp's context meter shows them: `1M`, `258K`, `950`.
fn compact_tokens(count: u64) -> String {
    match count {
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{}K", (n + 500) / 1_000),
        n => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
