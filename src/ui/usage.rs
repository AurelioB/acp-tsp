//! Usage overlay styled by Tern's built-in omp role CSS.
use tern_sdk::{
    Capabilities,
    ui::{self, Align, ChartKind, Gap, MeterStyle, Node, OverlaySize, Point, Size},
};

use super::{Msg, view::compact_tokens};
use crate::model::{Model, Turn};

pub fn render(model: &Model, caps: &Capabilities) -> Node<Msg> {
    let report = &model.usage_report;
    let checked: Node<Msg> = if report.fetching {
        ui::text("Checking…").key("checked").into()
    } else if report.pending {
        ui::text("Available after this turn").key("checked").into()
    } else if let Some(at) = report.checked {
        ui::row()
            .key("checked")
            .gap(Gap::Xs)
            .child(ui::text("checked"))
            .child(ui::elapsed().prop("age", at.elapsed().unwrap_or_default().as_millis() as u64))
            .child(ui::text("ago"))
            .into()
    } else {
        ui::text("Not checked yet").key("checked").into()
    };
    let head = ui::row()
        .key("head")
        .role("omp.usage.head")
        .gap(Gap::Sm)
        .align(Align::Center)
        .wrap(true)
        .child(checked)
        .child(ui::spacer().prop("grow", 1))
        .child(
            ui::tabs()
                .key("tabs")
                .tab("overview", "Overview")
                .tab("details", "Details")
                .active(
                    if report.details {
                        "details"
                    } else {
                        "overview"
                    }
                    .to_owned(),
                )
                .on_select(|id| Msg::UsageTab(id.to_owned())),
        )
        .child(action("refresh", "Refresh", "r", Msg::UsageRefresh))
        .child(action("close", "Close", "esc", Msg::UsageClose));
    let mut body = ui::col().key("body").gap(Gap::Sm).child(head);
    if report.details {
        let raw: Node<Msg> = if report.raw.is_empty() {
            ui::text("No agent report yet.").key("raw").into()
        } else {
            ui::ansi(report.raw.clone())
                .key("raw")
                .prop("max", serde_json::json!({ "h": "16lines" }))
                .into()
        };
        body = body.child(raw);
    } else {
        let title = model
            .agent
            .as_ref()
            .map_or(model.launch_name.as_str(), |agent| {
                agent.title.as_deref().unwrap_or(&agent.name)
            });
        let mut provider = ui::card().key("provider").role("omp.usage.provider").child(
            ui::row()
                .key("title")
                .role("omp.usage.provider.head")
                .gap(Gap::Sm)
                .child(ui::text(ui::span(title).style("strong")))
                .child(ui::text(
                    report
                        .plan
                        .as_deref()
                        .or_else(|| {
                            model
                                .ext
                                .auth
                                .as_ref()
                                .and_then(|auth| auth.account.as_ref())
                                .and_then(|account| account.plan.as_deref())
                        })
                        .unwrap_or("")
                        .to_owned(),
                )),
        );
        for (index, window) in report.windows.iter().enumerate() {
            let remaining = (100.0 - window.used_pct).clamp(0.0, 100.0);
            let mut row = ui::row()
                .key(format!("window.{index}"))
                .role("omp.usage.window")
                .gap(Gap::Sm)
                .align(Align::Center)
                .wrap(true)
                .child(
                    ui::text(short_label(&window.label))
                        .key("label")
                        .title(window.label.clone())
                        .role("omp.usage.label"),
                )
                .child(
                    ui::meter()
                        .key("bar")
                        .style(MeterStyle::Bar)
                        .value(window.used_pct / 100.0),
                )
                .child(
                    ui::text(format!("{remaining:.0}% left"))
                        .key("pct")
                        .role("omp.usage.pct"),
                );
            if !window.resets.is_empty() {
                row = row.child(
                    ui::text(window.resets.clone())
                        .key("reset")
                        .role("omp.usage.reset"),
                );
            }
            provider = provider.child(row);
        }
        if let Some(credits) = &report.credits {
            provider =
                provider.child(ui::text(crate::model::usage::credits_text(credits)).key("credits"));
        }
        if let Some(resets) = &report.resets {
            let count = if resets.available == 0 {
                "No resets available".to_owned()
            } else {
                format!("{} available", resets.available)
            };
            provider = provider.child(
                ui::row()
                    .key("resets")
                    .role("omp.usage.window")
                    .gap(Gap::Sm)
                    .child(ui::text("Available resets").role("omp.usage.label"))
                    .child(ui::text(count).role("omp.usage.pct")),
            );
            for (index, item) in resets.items.iter().take(3).enumerate() {
                let expiry = item.expires_at.map_or_else(String::new, |at| {
                    format!(" · expires {}", crate::model::usage::local_date(at))
                });
                let status = if item.status == "available" {
                    ""
                } else {
                    item.status.as_str()
                };
                provider = provider.child(
                    ui::text(format!(
                        "{}{}{}",
                        item.title,
                        if status.is_empty() {
                            String::new()
                        } else {
                            format!(" ({status})")
                        },
                        expiry
                    ))
                    .key(format!("reset.{index}"))
                    .role("omp.usage.reset"),
                );
            }
            let more = resets
                .available
                .max(resets.items.len() as u64)
                .saturating_sub(resets.items.len().min(3) as u64);
            if more > 0 && !resets.items.is_empty() {
                provider = provider.child(ui::text(format!("and {more} more")).key("reset.more"));
            }
        }
        if report.windows.is_empty() && report.resets.is_none() && report.credits.is_none() {
            provider = provider.child(
                ui::text(report.error.as_deref().unwrap_or("No limits reported yet"))
                    .key("empty")
                    .role("omp.usage.untouched"),
            );
        }
        let mut session = ui::card().key("session").role("omp.usage.provider").child(
            ui::text(ui::span("This session").style("strong"))
                .key("title")
                .role("omp.usage.provider.head"),
        );
        let completed = model.turn_summaries.len();
        let turns = completed
            + usize::from(
                matches!(model.turn, Turn::Running { .. })
                    && !(report.fetching && model.usage_command().is_some()),
            );
        let duration: std::time::Duration = model
            .turn_summaries
            .values()
            .map(|turn| turn.duration)
            .sum();
        let active = if let Turn::Running { since, .. } = model.turn {
            if report.fetching && model.usage_command().is_some() {
                std::time::Duration::ZERO
            } else {
                since.elapsed()
            }
        } else {
            std::time::Duration::ZERO
        };
        session = session.child(
            ui::text(format!(
                "{turns} turns · {:.1}s active",
                (duration + active).as_secs_f64()
            ))
            .key("turns"),
        );
        #[cfg(feature = "unstable_end_turn_token_usage")]
        {
            let (input, cached, output) = model
                .turn_summaries
                .values()
                .filter_map(|summary| summary.tokens.as_ref())
                .fold((0_u64, 0_u64, 0_u64), |(input, cached, output), usage| {
                    let cache = usage.cached_read_tokens.unwrap_or(0)
                        + usage.cached_write_tokens.unwrap_or(0);
                    (
                        input + usage.input_tokens + cache,
                        cached + cache,
                        output + usage.output_tokens,
                    )
                });
            session = session.child(
                ui::text(format!(
                    "{} in · {} cached · {} out",
                    compact_tokens(input),
                    compact_tokens(cached),
                    compact_tokens(output)
                ))
                .key("tokens"),
            );
        }
        if let Some(usage) = &model.content.usage {
            session = session.child(
                ui::text(format!(
                    "Context: {} / {} tokens",
                    compact_tokens(usage.used),
                    compact_tokens(usage.size)
                ))
                .key("context"),
            );
        }
        if let Some((currency, amount)) = session_cost(model) {
            session = session.child(
                ui::text(format!(
                    "Cost: {}{amount:.2}",
                    if currency == "USD" { "$" } else { &currency }
                ))
                .key("cost"),
            );
        }
        if super::supports(caps, "chart") && completed >= 2 {
            let mut turns: Vec<_> = model.turn_summaries.iter().collect();
            turns.sort_unstable_by_key(|(id, _)| *id);
            let mut bars = Vec::with_capacity(turns.len());
            let mut durations = Vec::with_capacity(turns.len());
            for (index, (_, turn)) in turns.iter().enumerate() {
                let seconds = turn.duration.as_secs_f64();
                durations.push(Point {
                    value: seconds,
                    title: Some(format!("Turn {}: {seconds:.1}s", index + 1)),
                    ..Point::default()
                });
                #[cfg(feature = "unstable_end_turn_token_usage")]
                let (value, title) = turn.tokens.as_ref().map_or_else(
                    || (0.0, format!("Turn {}: tokens unknown", index + 1)),
                    |usage| {
                        let cached = usage
                            .cached_read_tokens
                            .unwrap_or(0)
                            .saturating_add(usage.cached_write_tokens.unwrap_or(0));
                        let tokens = usage
                            .input_tokens
                            .saturating_add(cached)
                            .saturating_add(usage.output_tokens);
                        (
                            tokens as f64,
                            format!("Turn {}: {} tokens", index + 1, compact_tokens(tokens)),
                        )
                    },
                );
                #[cfg(not(feature = "unstable_end_turn_token_usage"))]
                let (value, title) = (seconds, format!("Turn {}: {seconds:.1}s", index + 1));
                bars.push(Point {
                    value,
                    title: Some(title),
                    ..Point::default()
                });
            }
            #[cfg(feature = "unstable_end_turn_token_usage")]
            let bar_summary = "Tokens per turn (input + output; unreported turns unknown)";
            #[cfg(not(feature = "unstable_end_turn_token_usage"))]
            let bar_summary = "Time per turn";
            session = session
                .child(
                    ui::chart(ChartKind::Bars)
                        .key("turn-bars")
                        .size(Size::Sm)
                        .series(bars)
                        .summary(bar_summary),
                )
                .child(
                    ui::chart(ChartKind::Spark)
                        .key("turn-durations")
                        .series(durations)
                        .summary("Turn durations (seconds)"),
                );
        }
        body = body.child(
            ui::row()
                .key("providers")
                .role("omp.usage.grid")
                .wrap(true)
                .gap(Gap::Md)
                .child(provider)
                .child(session),
        );
        let mut history = ui::col()
            .key("history")
            .role("omp.usage.activity")
            .gap(Gap::Sm)
            .child(
                ui::text(ui::span("Recorded on this machine by acp-tsp").style("strong"))
                    .key("history-title"),
            );
        if let Some(activity) = &report.history {
            if activity.turns == 0 {
                history = history.child(ui::text("No history yet").key("history-empty"));
            } else {
                if super::supports(caps, "chart") {
                    history = history.child(
                        ui::chart(ChartKind::Heatmap)
                            .key("history-heatmap")
                            .size(Size::Sm)
                            .token("accent")
                            .rows(["M", "", "W", "", "F", "", ""].map(str::to_owned).to_vec())
                            .cols(activity.cols.clone())
                            .cells(activity.cells.clone())
                            .tips(activity.tips.clone())
                            .summary(format!("{} turns · last 26 weeks", activity.turns)),
                    );
                }
                for (key, label, total) in [
                    ("today", "Today", &activity.today),
                    ("week", "Last 7 days", &activity.week),
                    ("month", "Last 30 days", &activity.month),
                ] {
                    history =
                        history.child(ui::text(format!("{label}: {}", total.label())).key(key));
                }
            }
        } else {
            history = history.child(ui::text("No history yet").key("history-empty"));
        }
        body = body.child(history);
    }
    ui::col()
        .child(
            ui::overlay()
                .key("usage")
                .head("Usage")
                .size(OverlaySize::Lg)
                .modal(true)
                .child(body),
        )
        .into()
}

pub fn session_cost(model: &Model) -> Option<(String, f64)> {
    model
        .content
        .usage
        .as_ref()
        .and_then(|usage| usage.cost.as_ref())
        .map(|cost| (cost.currency.clone(), cost.amount))
        .or_else(|| {
            let first = model
                .turn_summaries
                .values()
                .find_map(|summary| summary.cost.as_ref())?;
            let total = model
                .turn_summaries
                .values()
                .filter_map(|summary| summary.cost.as_ref())
                .filter(|(currency, _)| currency == &first.0)
                .map(|(_, amount)| amount)
                .sum();
            Some((first.0.clone(), total))
        })
}

fn action(key: &str, label: &str, shortcut: &str, message: Msg) -> Node<Msg> {
    ui::row()
        .key(key)
        .gap(Gap::Xs)
        .on_click(message)
        .child(ui::text(label))
        .child(ui::kbd([shortcut]))
        .into()
}

/// omp's limit column is narrow: "Weekly · all models" and "codex Weekly limit" both read
/// "Weekly"; the full label stays in the tooltip.
fn short_label(label: &str) -> String {
    let label = label.strip_prefix("codex ").unwrap_or(label);
    let label = label.replace(" · all models", "");
    let label = label
        .strip_suffix(" limit")
        .unwrap_or(&label)
        .trim()
        .to_owned();
    let mut chars = label.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn limit_labels_and_credits_read_compactly() {
        assert_eq!(short_label("codex Weekly limit"), "Weekly");
        assert_eq!(short_label("5h limit"), "5h");
        assert_eq!(short_label("Weekly · all models"), "Weekly");
        assert_eq!(short_label("5-hour limit"), "5-hour");
        assert_eq!(short_label("Weekly · Opus"), "Weekly · Opus");
        assert_eq!(
            crate::model::usage::credits_text("62372.8576375000"),
            "62,373 credits"
        );
        assert_eq!(
            crate::model::usage::credits_text("unlimited"),
            "Credits: unlimited"
        );
    }
    #[test]
    fn dashboard_charts_show_all_turns_and_26_weeks() {
        use std::time::Duration;
        use tern_sdk::{
            reconcile::{Doc, Tree},
            ui::View,
        };

        fn chart<'a>(node: &'a Tree, kind: &str) -> Option<&'a Tree> {
            if node.kind() == "chart"
                && node.props().get("kind").and_then(serde_json::Value::as_str) == Some(kind)
            {
                return Some(node);
            }
            node.children().iter().find_map(|child| chart(child, kind))
        }

        let mut model = Model::default();
        let caps = Capabilities {
            kinds: vec!["chart".into()],
            ..Capabilities::default()
        };
        for id in 1..=3 {
            model.turn_summaries.insert(
                id,
                crate::model::TurnSummary {
                    duration: Duration::from_secs(id),
                    #[cfg(feature = "unstable_end_turn_token_usage")]
                    tokens: None,
                    cost: None,
                    stop: None,
                },
            );
        }
        model.usage_report.history = Some(crate::usage_history::History {
            cells: vec![vec![Some(0.0); 26]; 7],
            tips: vec![vec![Some("0 tokens".into()); 26]; 7],
            cols: vec![tern_sdk::ui::ChartCol {
                at: 0,
                label: "Apr".into(),
            }],
            today: Default::default(),
            week: Default::default(),
            month: Default::default(),
            turns: 3,
        });
        let doc = Doc::from_view(View::new().layer(render(&model, &caps))).unwrap();
        let root = doc.regions()[2].as_ref().unwrap();
        for kind in ["bars", "spark"] {
            let series = chart(root, kind).unwrap().props()["series"]
                .as_array()
                .unwrap();
            assert_eq!(series.len(), 3);
        }
        let heatmap = chart(root, "heatmap").unwrap();
        let cells = heatmap.props()["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 7);
        assert!(cells.iter().all(|row| row.as_array().unwrap().len() == 26));
        assert!(
            chart(
                Doc::from_view(View::new().layer(render(&Model::default(), &caps)))
                    .unwrap()
                    .regions()[2]
                    .as_ref()
                    .unwrap(),
                "bars"
            )
            .is_none()
        );
    }
}
