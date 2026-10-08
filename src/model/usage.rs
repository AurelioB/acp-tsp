//! Local adapter usage commands: their markdown is display data, not transcript content.
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    pub label: String,
    /// Percent of the limit consumed (Claude reports used; Codex reports left).
    pub used_pct: f64,
    pub resets: String,
}

#[derive(Debug, Default)]
pub struct UsageReport {
    pub open: bool,
    pub details: bool,
    pub pending: bool,
    pub fetching: bool,
    pub checked: Option<SystemTime>,
    pub raw: String,
    pub windows: Vec<LimitWindow>,
    pub credits: Option<String>,
    pub plan: Option<String>,
    pub error: Option<String>,
}

/// Returns `None` when the format cannot be identified. Keep raw markdown for Details.
pub fn parse(
    markdown: &str,
    command: &str,
) -> Option<(Vec<LimitWindow>, Option<String>, Option<String>)> {
    let mut windows = Vec::new();
    let mut credits = None;
    let mut plan = None;
    for line in markdown.lines().map(str::trim) {
        if command == "/usage" {
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
        } else if command == "/status" {
            if let Some((label, value)) = line
                .strip_prefix("**")
                .and_then(|s| s.split_once("Credits:**"))
                && (label.is_empty() || label.ends_with(' '))
            {
                credits = Some(value.trim().to_owned());
            }
            if let Some(value) = line.strip_prefix("**Account:**") {
                plan = Some(
                    value
                        .trim()
                        .split_once(" (")
                        .map_or(value.trim(), |(plan, _)| plan)
                        .to_owned(),
                );
            }
            if let Some((label, rest)) = line.strip_prefix("**").and_then(|s| s.split_once(":**"))
                && label.to_ascii_lowercase().contains("limit")
                && let Some((pct, suffix)) = rest.trim().split_once("% left")
                && let Ok(left) = pct.trim().parse::<f64>()
                && (0.0..=100.0).contains(&left)
            {
                windows.push(LimitWindow {
                    label: label.to_owned(),
                    used_pct: 100.0 - left,
                    resets: suffix
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .to_owned(),
                });
            }
        }
    }
    (!windows.is_empty()).then_some((windows, credits, plan))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_captured_usage() {
        let raw = include_str!("../../tests/fixtures/claude/usage.md");
        let (windows, credits, plan) = parse(raw, "/usage").expect("Claude limits");
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
    fn codex_captured_status() {
        let raw = include_str!("../../tests/fixtures/codex/status.md");
        let (windows, credits, plan) = parse(raw, "/status").expect("Codex limits");
        assert_eq!(
            windows,
            vec![LimitWindow {
                label: "codex Weekly limit".into(),
                used_pct: 44.0,
                resets: "resets 21:28 on Oct 13".into(),
            }]
        );
        assert_eq!(credits.as_deref(), Some("62372.8576375000"));
        assert_eq!(plan.as_deref(), Some("ChatGPT pro"));
    }

    #[test]
    fn codex_primary_and_secondary_windows_use_percent_left() {
        let raw = "**5h limit:** 92% left (resets 14:00)  \n**Weekly limit:** 76% left (resets 21:28 on 14 Oct)";
        let (windows, _, _) = parse(raw, "/status").expect("Codex limits");
        assert_eq!(windows[0].used_pct, 8.0);
        assert_eq!(windows[0].resets, "resets 14:00");
        assert_eq!(windows[1].used_pct, 24.0);
        assert_eq!(windows[1].resets, "resets 21:28 on 14 Oct");
    }

    #[test]
    fn malformed_report_keeps_raw_without_inventing_limits() {
        assert!(parse("**Limits:** data not available yet", "/status").is_none());
        assert!(parse("## Usage\n\n### This session", "/usage").is_none());
    }
}
