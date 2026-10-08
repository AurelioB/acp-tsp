# Support matrix

Versions:
- ACP: protocol v1 via `agent-client-protocol` 3.1.0.
- Tern: 0.6.2, headless `tern serve` for the live checks.
- Agents: `@agentclientprotocol/claude-agent-acp` 0.87.0, `@agentclientprotocol/codex-acp` 2.1.1, agy `agy_acp_server` 1.3.0 (registry `antigravity-acp`).

Status meanings:
- **live**: exercised with the real agent in headless Tern (`scripts/tern-e2e.sh`) on 2026-10-08, checked through screenshots and `tern ctl tree`. Headless input is key presses, so native text editing in a real window is covered only by `tests/ui_sessions.rs::native_edits_build_the_prompt_and_enter_sends_it_once`. Narrow windows are untested.
- **fixture**: a recorded real session replayed in `tests/replay_agents.rs`.
- **fake**: an in-process fake agent in `tests/acp_*.rs` or a scripted terminal in `tests/ui_*.rs`. It proves acp-tsp's side only.
- **n/a (agent)**: the agent never uses the feature. Sources are listed in `docs/research/agents.md`.
- **not implemented**: acp-tsp does not implement the feature.
- **unknown**: never observed.

| Capability | acp-tsp | Claude | Codex | agy |
|---|---|---|---|---|
| initialize, session/new, prompt streaming | yes | live | live | unknown (server dies with SIGILL on this host) |
| authenticate (agent methods) | yes | untested (already logged in) | untested (already logged in) | unknown |
| terminal auth methods, logout | yes | fake | fake | unknown |
| thought chunks | yes | fake | fake | unknown |
| tool call: file read | yes | live | n/a (agent): reads run as shell commands | unknown |
| tool call: edit with diff | yes | live | untested | unknown |
| tool call: shell command with output | yes | live (`Bash`) | live (`exec_command`, `tests/ui_view.rs::codex_exec_command_*`) | unknown |
| `_meta.terminal_*` output streaming | yes | live | live | unknown |
| session/request_permission | yes | fake (bypass mode here) | live (read-only mode) | unknown |
| session/cancel, mid-turn stop | yes | live (Escape and stop button) | live (Escape) | unknown |
| fs/read_text_file, fs/write_text_file | yes | fake (not observed live) | n/a (agent) | unknown |
| terminal/* (client terminals) | yes | n/a (agent) | n/a (agent) | unknown |
| session/list + load | yes | live | fake | unknown |
| session/resume, close, delete, fork | yes | fake | fake | unknown |
| modes / config options / model picker | yes | live (chips, sheet) | live (Shift+Tab mode, picker) | unknown |
| slash commands, `@` mentions | yes | live | live (`/`) | unknown |
| usage dashboard (hidden local command, limits, session totals) | yes | live (`/usage`) | live (`/status`) | unknown |
| elicitation: form | yes | live (AskUserQuestion) | live in Plan mode (`request_user_input`) | unknown |
| elicitation: URL | yes | fake | fake (device-code login uses it) | unknown |
| usage_update (context, cost) | yes | live | live (context only; ChatGPT plans report no cost) | unknown |
| end-turn token usage (single model request per turn) | yes | live, fixture | live, fixture | unknown |
| end-turn token usage (several model requests in one turn) | shows what the agent reports | fixture (`read_and_list`: one turn's usage spans two models; not checked against per-request totals) | untested | unknown |
| plan / plan_update | yes | fake | fixture (`read_only_plan`) | unknown |
| plan approval (ExitPlanMode, Codex plan review) | yes | fake | fake | unknown |
| steering (`_session/steering`) | yes | live | disabled: the adapter ignores `idleBehavior` (`quirks::steering`) | unknown |
| `_auth/status_update` account chip | yes | live | live | unknown |
| `_claude/rateLimit` banner | yes | fake | n/a | n/a |
| Codex 5h/weekly limits | yes | n/a | weekly live; 5h format supported (adapter source) | n/a |
| goal / async tasks | no | n/a (agent): JetBrains AIR clients only | n/a (agent): JetBrains AIR clients only | n/a |
| notices, compaction, subagents, providers (unstable) | yes | fake | fake | unknown |
| configured MCP servers | yes | fake | fake | unknown |
| MCP-over-ACP `tern` server | new sessions only (SDK limit) | n/a (agent): no `mcpCapabilities.acp` | n/a (agent): `acp: false` | unknown |
| `$/cancel_request` | yes | fake | fake | unknown |
| NES (`nes/*`, `document/*`), draft protocol v2 | not implemented | — | — | — |

Other ACP agents (Gemini CLI, OpenCode, Goose, …) start via `acp-tsp -- <command>` but are **untested**.

Usage numbers come from `PromptResponse.usage`, which ACP defines as per-turn usage. "In" counts uncached input plus cache reads and writes. Each turn re-sends the whole conversation, so tens of thousands of input tokens on one turn are normal; the cached share shows how much of that was cheap. When an agent reports no usage, only the duration is shown. How the adapter sums several model requests within one turn is its own responsibility. acp-tsp displays the reported totals and has not compared them with per-request numbers.

The client `/usage` dashboard queries the active adapter's local command only when idle. Claude's
`/usage` reports percent **used**; Codex's `/status` reports percent **left**. Both are normalized
to a used-fraction bar labeled with percent left. Query turns are hidden from transcript and per-session turn totals.
The adapter's raw markdown remains available in Details (including account information).
Only these two adapters are queried, and only when they advertise the command: both answer it without a model call. Another agent's `/usage` could be an ordinary model prompt, so other agents show session totals only.

Why parse text at all: Codex keeps its rate limits inside the adapter and only renders them in `/status` text. Claude does stream structured `_meta["_claude/rateLimit"]` events (`rateLimitType`, `utilization`, `resetsAt`). But each event covers one window, and they arrive only when the SDK emits a rate-limit event, so they can't build the full set of limits on demand. acp-tsp uses them for the rate-limit warning banner, and Claude's `/usage` for the dashboard. The text parsers are pinned to claude-agent-acp 0.87.0 and codex-acp 2.1.1, tested against captured output (`tests/fixtures/{claude/usage.md,codex/status.md}`). If the wording changes, no limit rows are shown and the raw report stays in Details. Values are never carried over from an earlier check, and "checked … ago" shows how old they are.

Fixture tests: `tests/replay_agents.rs` with `tests/fixtures/{claude,codex}/*.jsonl`. Footer arithmetic: `src/ui/view.rs::tests::turn_footer_counts_cached_context_as_input`.
