//! Adapter extensions: parse only fields with known UI semantics, never log payloads.
use std::collections::HashMap;

use agent_client_protocol::schema::v1::{Meta, ToolCallId};
use serde::Deserialize;
use serde_json::Value;

const OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Steering {
    pub supported: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GoalCapability {
    pub control_method: Option<String>,
    pub actions: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Account {
    pub email: Option<String>,
    pub organization: Option<String>,
    pub plan: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AuthStatus {
    pub kind: Option<String>,
    pub label: Option<String>,
    pub detail: Option<String>,
    pub account: Option<Account>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AuthUpdate {
    #[serde(rename = "authStatus")]
    auth_status: Option<AuthStatus>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TerminalInfo {
    terminal_id: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OutputChunk {
    terminal_id: Option<String>,
    data: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TerminalExit {
    terminal_id: Option<String>,
    exit_code: Option<i32>,
    signal: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ClaudeTool {
    tool_name: Option<String>,
    parent_tool_use_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TokenCount {
    #[serde(rename = "totalTokens")]
    total_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Quota {
    token_count: Option<TokenCount>,
}

#[derive(Debug, Default)]
pub struct ToolMeta {
    pub terminal_id: Option<String>,
    pub cwd: Option<String>,
    pub output: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub mcp: bool,
    pub mcp_output: String,
    pub tool_name: Option<String>,
    pub parent: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct ExtensionState {
    pub steering: bool,
    pub auth_advertised: bool,
    pub auth: Option<AuthStatus>,
    pub account_details: bool,
    pub goal: Option<GoalCapability>,
    pub goal_open: bool,
    pub goal_draft: String,
    pub tools: HashMap<ToolCallId, ToolMeta>,
    pub quota_tokens: Option<u64>,
    pub rate_limit: Option<String>,
    pub toast: Option<String>,
}

fn parse<T: for<'de> Deserialize<'de>>(value: &Value) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}

fn field<T: for<'de> Deserialize<'de>>(meta: &Meta, key: &str) -> Option<T> {
    meta.get(key).and_then(parse)
}

fn append_bounded(output: &mut String, text: &str, truncated: &mut bool) {
    output.push_str(text);
    if output.len() > OUTPUT_LIMIT {
        let mut start = output.len() - OUTPUT_LIMIT;
        while !output.is_char_boundary(start) {
            start += 1;
        }
        output.drain(..start);
        *truncated = true;
    }
}

impl ExtensionState {
    pub fn initialized(&mut self, init: &agent_client_protocol::schema::v1::InitializeResponse) {
        self.steering = init
            .meta
            .as_ref()
            .and_then(|m| field::<Steering>(m, "steering"))
            .is_some_and(|s| s.supported);
        self.auth_advertised = init
            .agent_capabilities
            .meta
            .as_ref()
            .is_some_and(|m| m.contains_key("authStatus"));
        self.goal = init
            .meta
            .as_ref()
            .and_then(|m| m.get("jetbrains"))
            .and_then(|m| m.get("air"))
            .and_then(|m| m.get("goal"))
            .and_then(parse::<GoalCapability>)
            .filter(|goal| goal.control_method.as_deref() == Some("_session/goal"));
    }

    pub fn notification(&mut self, method: &str, params: &Value) {
        match method {
            "_auth/status_update" => {
                if let Some(update) = parse::<AuthUpdate>(params) {
                    self.auth = update.auth_status;
                }
            }
            "_claude/rateLimit" => self.rate(params),
            _ => {}
        }
    }

    pub fn rate(&mut self, rate: &Value) {
        // Claude SDK reports `resetsAt` in Unix seconds, alongside
        // allowed/allowed_warning/rejected. Don't warn for an allowed state.
        let status = rate.get("status").and_then(Value::as_str);
        if status == Some("allowed") {
            self.rate_limit = None;
            return;
        }
        let reset = rate
            .get("resetsAt")
            .or_else(|| rate.get("resets_at"))
            .and_then(|value| {
                value
                    .as_u64()
                    .map(|seconds| {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let remaining = seconds.saturating_sub(now);
                        if remaining >= 3600 {
                            format!(
                                "resets in {}h {}m",
                                remaining / 3600,
                                (remaining % 3600) / 60
                            )
                        } else {
                            format!("resets in {}m", remaining.div_ceil(60))
                        }
                    })
                    .or_else(|| value.as_str().map(str::to_owned))
            });
        let title = if status == Some("allowed_warning") {
            "Approaching rate limit"
        } else {
            "Rate limit reached"
        };
        self.rate_limit =
            Some(reset.map_or_else(|| title.to_owned(), |reset| format!("{title} · {reset}")));
    }

    pub fn usage(&mut self, meta: Option<&Meta>) {
        if let Some(meta) = meta {
            if let Some(rate) = meta.get("_claude/rateLimit") {
                self.rate(rate);
            }
            if let Some(quota) = field::<Quota>(meta, "quota") {
                self.quota_tokens = quota.token_count.and_then(|count| count.total_tokens);
            }
        }
    }

    pub fn tool(&mut self, id: &ToolCallId, meta: Option<&Meta>) {
        let Some(meta) = meta else {
            return;
        };
        let tool = self.tools.entry(id.clone()).or_default();
        if let Some(claude) = field::<ClaudeTool>(meta, "claudeCode") {
            if claude.tool_name.is_some() {
                tool.tool_name = claude.tool_name;
            }
            if claude.parent_tool_use_id.is_some() {
                tool.parent = claude.parent_tool_use_id;
            }
        }
        if let Some(info) = field::<TerminalInfo>(meta, "terminal_info") {
            if info.terminal_id.is_some() {
                tool.terminal_id = info.terminal_id;
            }
            if info.cwd.is_some() {
                tool.cwd = info.cwd;
            }
        }
        if meta.get("is_mcp_tool_call").and_then(Value::as_bool) == Some(true) {
            tool.mcp = true;
        }
        if let Some(error) = meta.get("codex").and_then(|codex| codex.get("error")) {
            tool.error = Some(
                error
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.to_string()),
            );
        }
        if let Some(chunk) = field::<OutputChunk>(meta, "terminal_output") {
            if chunk.terminal_id.is_some() {
                tool.terminal_id = chunk.terminal_id;
            }
            if let Some(output) = chunk.data {
                // Zed's terminal_output is emitted in chunks by both adapters,
                // while some peers send full snapshots. A growing prefix is a
                // snapshot; otherwise this is the next output fragment.
                if output.starts_with(&tool.output) && !tool.truncated {
                    tool.output.clear();
                    append_bounded(&mut tool.output, &output, &mut tool.truncated);
                } else {
                    append_bounded(&mut tool.output, &output, &mut tool.truncated);
                }
            }
        }
        if let Some(chunk) = field::<OutputChunk>(meta, "terminal_output_delta") {
            if chunk.terminal_id.is_some() {
                tool.terminal_id = chunk.terminal_id;
            }
            if let Some(output) = chunk.data {
                append_bounded(&mut tool.output, &output, &mut tool.truncated);
            }
        }
        if let Some(chunk) = field::<OutputChunk>(meta, "mcp_output_delta")
            && let Some(output) = chunk.data
        {
            append_bounded(&mut tool.mcp_output, &output, &mut tool.truncated);
        }
        if let Some(exit) = field::<TerminalExit>(meta, "terminal_exit") {
            if exit.terminal_id.is_some() {
                tool.terminal_id = exit.terminal_id;
            }
            tool.exit_code = exit.exit_code;
            tool.signal = exit.signal;
        }
    }
}
