# Support matrix

Versions:
- ACP: protocol v1 via `agent-client-protocol` 3.1.0.
- Tern: 0.6.2, headless `tern serve` for the live checks.
- Agents: `@agentclientprotocol/claude-agent-acp` 0.87.0, `@agentclientprotocol/codex-acp` 2.1.1, agy `agy_acp_server` 1.3.0 (registry `antigravity-acp`).

Status meanings:
- **live**: exercised with the real agent in Tern on 2026-10-08.
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
| tool calls (read, edit with diff, shell) | yes | live | live (`exec_command` shell cards) | unknown |
| `_meta.terminal_*` output streaming | yes | live | live | unknown |
| session/request_permission | yes | fake (bypass mode here) | live (read-only mode) | unknown |
| session/cancel, mid-turn stop | yes | live (Escape and stop button) | live (Escape) | unknown |
| fs/read_text_file, fs/write_text_file | yes | fake (not observed live) | n/a (agent) | unknown |
| terminal/* (client terminals) | yes | n/a (agent) | n/a (agent) | unknown |
| session/list + load | yes | live | fake | unknown |
| session/resume, close, delete, fork | yes | fake | fake | unknown |
| modes / config options / model picker | yes | live (chips, sheet) | live (Shift+Tab mode, picker) | unknown |
| slash commands, `@` mentions | yes | live | live (`/`) | unknown |
| elicitation: form | yes | live (AskUserQuestion) | fake | unknown |
| elicitation: URL | yes | fake | fake (device-code login uses it) | unknown |
| usage_update (context, cost) | yes | live | live (context only; ChatGPT plans report no cost) | unknown |
| end-turn token usage | yes | live, fixture | live, fixture | unknown |
| plan / plan_update | yes | fake | fixture (`read_only_plan`) | unknown |
| plan approval (ExitPlanMode, Codex plan review) | yes | fake | fake | unknown |
| steering (`_session/steering`) | yes | live | disabled: the adapter ignores `idleBehavior` (`quirks::steering`) | unknown |
| `_auth/status_update` account chip | yes | live | live | unknown |
| `_claude/rateLimit` banner | yes | fake | n/a | n/a |
| Codex 5h/weekly limits | no | n/a | n/a (agent): only `/status` text, no data | n/a |
| goal / async tasks | no | n/a (agent): JetBrains AIR clients only | n/a (agent): JetBrains AIR clients only | n/a |
| notices, compaction, subagents, providers (unstable) | yes | fake | fake | unknown |
| configured MCP servers | yes | fake | fake | unknown |
| MCP-over-ACP `tern` server | new sessions only (SDK limit) | n/a (agent): no `mcpCapabilities.acp` | n/a (agent): `acp: false` | unknown |
| `$/cancel_request` | yes | fake | fake | unknown |
| NES (`nes/*`, `document/*`), draft protocol v2 | not implemented | — | — | — |

Other ACP agents (Gemini CLI, OpenCode, Goose, …) start via `acp-tsp -- <command>` but are **untested**.

Usage numbers come from `PromptResponse.usage`, which ACP defines as per-turn usage. "In" counts uncached input plus cache reads and writes. Each turn re-sends the whole conversation, so tens of thousands of input tokens on one turn are normal; the cached share shows how much of that was cheap. When an agent reports no usage, only the duration is shown.
