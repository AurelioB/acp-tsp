//! Usage overlay styled by Tern's built-in omp role CSS.
use tern_sdk::ui::{self, Align, Gap, MeterStyle, Node, OverlaySize};

use super::{Msg, view::compact_tokens};
use crate::model::{Model, Turn};

pub fn render(model: &Model) -> Node<Msg> {
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
            ui::md(report.raw.clone()).key("raw").into()
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
            provider = provider.child(ui::text(credits_text(credits)).key("credits"));
        }
        if report.windows.is_empty() {
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
        let turns =
            completed + usize::from(matches!(model.turn, Turn::Running { .. }) && !report.fetching);
        let duration: std::time::Duration = model
            .turn_summaries
            .values()
            .map(|turn| turn.duration)
            .sum();
        let active = if let Turn::Running { since, .. } = model.turn {
            if report.fetching {
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
        body = body.child(
            ui::row()
                .key("providers")
                .role("omp.usage.grid")
                .wrap(true)
                .gap(Gap::Md)
                .child(provider)
                .child(session),
        );
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

/// `62372.8576375000` → `62,373 credits`; anything unparsable is shown as reported.
fn credits_text(raw: &str) -> String {
    let Ok(value) = raw.trim().parse::<f64>() else {
        return format!("Credits: {raw}");
    };
    let whole = value.round() as u64;
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{grouped} credits")
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
        assert_eq!(credits_text("62372.8576375000"), "62,373 credits");
        assert_eq!(credits_text("unlimited"), "Credits: unlimited");
    }
}
