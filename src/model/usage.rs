//! Adapter usage data: Claude's local markdown and Codex's structured rate limits.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    pub label: String,
    /// Percent of the limit consumed.
    pub used_pct: f64,
    pub resets: String,
}

#[derive(Debug)]
pub struct ResetCredit {
    pub title: String,
    pub expires_at: Option<SystemTime>,
    pub status: String,
}

#[derive(Debug)]
pub struct ResetCredits {
    pub available: u64,
    pub items: Vec<ResetCredit>,
}

#[derive(Debug)]
pub struct CodexUsage {
    pub windows: Vec<LimitWindow>,
    pub credits: Option<String>,
    pub plan: Option<String>,
    pub resets: Option<ResetCredits>,
    /// Human-readable only; never contains opaque credit/account/limit ids.
    pub summary: String,
}

#[derive(Debug, Default)]
pub struct UsageReport {
    pub open: bool,
    pub details: bool,
    pub pending: bool,
    pub fetching: bool,
    /// Advances on each read and on invalidation; discards stale sidecar responses.
    pub generation: u64,
    pub checked: Option<SystemTime>,
    pub raw: String,
    pub windows: Vec<LimitWindow>,
    pub credits: Option<String>,
    pub plan: Option<String>,
    pub resets: Option<ResetCredits>,
    pub error: Option<String>,
}

/// Claude's local `/usage` markdown. Returns `None` when limits are absent.
pub fn parse(markdown: &str) -> Option<(Vec<LimitWindow>, Option<String>, Option<String>)> {
    let mut windows = Vec::new();
    let mut plan = None;
    for line in markdown.lines().map(str::trim) {
        if let Some(subscription) = line
            .strip_prefix("> Claude ")
            .and_then(|s| s.strip_suffix(" subscription usage"))
        {
            plan = Some(subscription.to_owned());
        }
        if let Some((label, rest)) = line
            .strip_prefix("**")
            .and_then(|s| s.split_once("** — **"))
            && let Some((pct, suffix)) = rest.split_once("%**")
            && let Ok(used_pct) = pct.parse::<f64>()
            && (0.0..=100.0).contains(&used_pct)
        {
            windows.push(LimitWindow {
                label: label.replace("\\*", "*").replace("\\_", "_"),
                used_pct,
                resets: suffix.trim().trim_start_matches('·').trim().to_owned(),
            });
        }
    }
    (!windows.is_empty()).then_some((windows, None, plan))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResponse {
    rate_limits: Snapshot,
    rate_limits_by_limit_id: Option<std::collections::BTreeMap<String, Snapshot>>,
    rate_limit_reset_credits: Option<ResetResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    limit_id: String,
    limit_name: Option<String>,
    primary: Option<Window>,
    secondary: Option<Window>,
    credits: Option<Credits>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    used_percent: u8,
    window_duration_mins: Option<u64>,
    resets_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credits {
    balance: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetResponse {
    available_count: u64,
    credits: Option<Vec<ResetItem>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetItem {
    title: Option<String>,
    expires_at: Option<i64>,
    status: String,
}

pub fn local_date(time: SystemTime) -> String {
    let date: DateTime<Local> = time.into();
    date.format("%b %-d, %-I:%M %p %Z").to_string()
}

/// `62372.8576375000` → `62,373 credits`; anything unparsable is shown as reported.
pub fn credits_text(raw: &str) -> String {
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

fn timestamp(seconds: i64) -> Option<SystemTime> {
    // Chrono's supported range is narrower than SystemTime's; never panic while formatting.
    DateTime::<chrono::Utc>::from_timestamp(seconds, 0)?;
    if seconds >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(seconds.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
    }
}

fn duration_label(minutes: Option<u64>) -> Option<String> {
    minutes.map(|minutes| {
        if minutes == 10_080 {
            "Weekly".into()
        } else if minutes % 1_440 == 0 {
            format!("{}d", minutes / 1_440)
        } else if minutes % 60 == 0 {
            format!("{}h", minutes / 60)
        } else {
            format!("{minutes}m")
        }
    })
}

fn add_window(
    windows: &mut Vec<LimitWindow>,
    snapshot: &Snapshot,
    window: &Window,
) -> Result<(), String> {
    if window.used_percent > 100 {
        return Err("Invalid Codex limit percentage".into());
    }
    let name = snapshot.limit_name.as_deref().unwrap_or_else(|| {
        if !snapshot.limit_id.is_empty()
            && snapshot.limit_id.len() <= 32
            && snapshot
                .limit_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            &snapshot.limit_id
        } else {
            "Limit"
        }
    });
    let duration = duration_label(window.window_duration_mins);
    let label = match duration {
        Some(duration) if !name.eq_ignore_ascii_case(&duration) => format!("{name} · {duration}"),
        Some(_) | None => name.to_owned(),
    };
    let resets = window
        .resets_at
        .and_then(timestamp)
        .map_or_else(String::new, |time| format!("Resets {}", local_date(time)));
    windows.push(LimitWindow {
        label,
        used_pct: f64::from(window.used_percent),
        resets,
    });
    Ok(())
}

/// Parse only the JSON-RPC result, dropping account ids, credit ids and all unneeded fields.
/// Never include the source JSON or serde diagnostics in returned errors.
pub fn parse_codex(value: serde_json::Value) -> Result<CodexUsage, String> {
    let response: RateLimitsResponse = serde_json::from_value(value)
        .map_err(|_| "Unexpected Codex rate-limits response".to_owned())?;
    let mut windows = Vec::new();
    let snapshots: Vec<&Snapshot> = match &response.rate_limits_by_limit_id {
        Some(by_id) if !by_id.is_empty() => by_id.values().collect(),
        _ => vec![&response.rate_limits],
    };
    for snapshot in &snapshots {
        if let Some(primary) = &snapshot.primary {
            add_window(&mut windows, snapshot, primary)?;
        }
        if let Some(secondary) = &snapshot.secondary {
            add_window(&mut windows, snapshot, secondary)?;
        }
    }
    let credits = response
        .rate_limits
        .credits
        .as_ref()
        .and_then(|credits| credits.balance.clone())
        .or_else(|| {
            snapshots
                .iter()
                .find_map(|snapshot| snapshot.credits.as_ref().and_then(|c| c.balance.clone()))
        });
    let plan = response.rate_limits.plan_type.clone().or_else(|| {
        snapshots
            .iter()
            .find_map(|snapshot| snapshot.plan_type.clone())
    });
    let resets = response.rate_limit_reset_credits.map(|reset| ResetCredits {
        available: reset.available_count,
        items: reset
            .credits
            .unwrap_or_default()
            .into_iter()
            .map(|credit| ResetCredit {
                title: credit
                    .title
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| "Reset credit".into()),
                expires_at: credit.expires_at.and_then(timestamp),
                status: credit.status,
            })
            .collect(),
    });
    let mut lines = Vec::new();
    if let Some(plan) = &plan {
        lines.push(format!("Plan: {plan}"));
    }
    for window in &windows {
        lines.push(format!(
            "{}: {:.0}% used{}",
            window.label,
            window.used_pct,
            if window.resets.is_empty() {
                String::new()
            } else {
                format!(" · {}", window.resets)
            }
        ));
    }
    if let Some(credits) = &credits {
        lines.push(format!("Credits balance: {}", credits_text(credits)));
    }
    if let Some(resets) = &resets {
        lines.push(format!("Available resets: {}", resets.available));
        for item in &resets.items {
            lines.push(format!(
                "{} ({}){}",
                item.title,
                item.status,
                item.expires_at
                    .map_or_else(String::new, |at| format!(" · expires {}", local_date(at)))
            ));
        }
    }
    Ok(CodexUsage {
        windows,
        credits,
        plan,
        resets,
        summary: lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_captured_usage() {
        let raw = include_str!("../../tests/fixtures/claude/usage.md");
        let (windows, credits, plan) = parse(raw).expect("Claude limits");
        assert_eq!(windows.len(), 3);
        assert_eq!(
            windows[0],
            LimitWindow {
                label: "5-hour limit".into(),
                used_pct: 9.0,
                resets: "Resets Oct 8, 3:50 PM MDT".into(),
            }
        );
        assert_eq!(windows[1].used_pct, 52.0);
        assert_eq!(windows[2].used_pct, 0.0);
        assert_eq!(credits, None);
        assert_eq!(plan.as_deref(), Some("team"));
    }

    #[test]
    fn codex_structured_limits_and_resets() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/codex/rate_limits.json"))
                .unwrap();
        let report = parse_codex(fixture.clone()).unwrap();
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].used_pct, 44.0);
        assert_eq!(report.windows[1].used_pct, 9.0);
        assert!(report.windows[0].label.contains("5h"));
        assert!(report.windows[1].label.contains("Weekly"));
        assert_eq!(report.credits.as_deref(), Some("62372.8576375000"));
        assert_eq!(report.plan.as_deref(), Some("pro"));
        let resets = report.resets.unwrap();
        assert_eq!(resets.available, 3);
        assert_eq!(resets.items.len(), 2); // The list may be capped below the count.
        assert!(resets.items[0].expires_at.is_some());
        assert!(!report.summary.contains("fake-account"));
        assert!(!report.summary.contains("fake-credit"));
        let mut count_only = fixture.clone();
        count_only["rateLimitResetCredits"]["credits"] = serde_json::Value::Null;
        let count_only = parse_codex(count_only).unwrap().resets.unwrap();
        assert_eq!(count_only.available, 3);
        assert!(count_only.items.is_empty());
        let mut empty = fixture.clone();
        empty["rateLimitResetCredits"]["availableCount"] = 0.into();
        empty["rateLimitResetCredits"]["credits"] = serde_json::json!([]);
        assert_eq!(parse_codex(empty).unwrap().resets.unwrap().available, 0);
        let mut unnamed = fixture.clone();
        unnamed["rateLimits"]["limitName"] = serde_json::Value::Null;
        assert_eq!(
            parse_codex(unnamed.clone()).unwrap().windows[0].label,
            "codex · 5h"
        );
        unnamed["rateLimits"]["limitId"] = "private:value".into();
        assert_eq!(parse_codex(unnamed).unwrap().windows[0].label, "Limit · 5h");
        let mut unknown = fixture;
        unknown["rateLimitResetCredits"] = serde_json::Value::Null;
        assert!(parse_codex(unknown).unwrap().resets.is_none());
    }

    #[test]
    fn malformed_report_keeps_raw_without_inventing_limits() {
        assert!(parse("## Usage\n\n### This session").is_none());
        assert!(parse_codex(serde_json::json!({"rateLimits": {}})).is_err());
    }
}
