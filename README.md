# acp-tsp

`acp-tsp` is an ACP client that runs in a Tern agent block and renders the conversation natively with Tern Surface Protocol (TSP).

Status:

- Stage 1 (basics) done: prompts, streamed answers/thoughts, tool calls with diffs, plans, permission dialogs, cancel, agent auth, confined `fs/*`, restart.
- Stage 2 (rest of ACP v1 + unstable features) done: `terminal/*`, session list/load/resume/close/delete/fork, `additionalDirectories`, modes and config options, slash commands, file mentions with embedded context/images/audio, MCP servers, form/URL elicitation, terminal auth, logout, usage, `$/cancel_request`, providers, plan operations, notices, compaction, subagents, end-turn usage, MCP-over-ACP (`tern_open` tool). Not implemented: NES (`nes/*`, `document/*`) and draft protocol v2 — see [docs/PLAN.md](docs/PLAN.md) §10 D2.
- Stage 3 (agent-specific extensions) done: `_meta.terminal_*` command output streaming, `_auth/status_update` account chip, steering (`_session/steering`, Claude), Claude tool names / nested subagent tools / ExitPlanMode plan approval / rate-limit banner, Codex plan review / API-key env auth / error meta, adapter `session_meta`, agy registry install + auth helper, redacted ACP recording (`ACP_TSP_RECORD`) and replay tests from real Claude/Codex sessions. AIR-only adapter features (goal, async tasks) are not offered because acp-tsp does not identify as JetBrains AIR.

## Requirements and installation

Use a Tern **agent block** (surfaces do not render in shell blocks). Node.js/npm is required for the Claude and Codex `npx` adapters.

```sh
cargo install --path .
acp-tsp --agent claude
acp-tsp --agent codex --cwd /path/to/project
acp-tsp -- npx -y some-acp-agent --option
acp-tsp agents
acp-tsp setup-tern --agent claude
```

Resume earlier sessions from the command line (any agent that can load or resume sessions):

```sh
acp-tsp --agent codex --resume        # pick one of this directory's sessions
acp-tsp --agent codex --resume ID     # reopen ID, replaying its history when the agent can
acp-tsp --agent codex --continue      # the most recently updated session here
```

On quit, acp-tsp prints the exact `--resume` command for the open session. Closing the
`--resume` picker starts a new session; agents that cannot list sessions start a new one with a
notice. `--resume` and `--continue` are mutually exclusive. Repeat `--add-dir DIR` to expose
additional existing workspace directories when the agent supports them.

The pane follows the session: its title is the session title (the agent's name until there is
one, OSC 2), Tern's working indicator runs during a turn and pauses while a permission request,
question or sign-in waits (OSC 9;4), and Tern's attention inbox gets "Permission needed",
"Question waiting", "Stopped with error" and "Agent exited" notices (OSC 99). A finished turn
is the progress clearing: in an agent block Tern records it as `agent-done` itself. Cancelled
turns and hidden usage queries stay quiet.

`setup-tern` updates `agent_command` in Tern's settings, making a timestamped backup first. It leaves `new_blocks` unchanged unless passed `--new-blocks-agent`; `--settings PATH` overrides settings location and `--print` prints the JSON snippet without writing. On Linux the default settings location is `$XDG_CONFIG_HOME/tern/settings.json` (or `~/.config/tern/settings.json`), a best-known default that may vary by Tern installation. macOS uses `~/Library/Application Support/Tern/settings.json`.

## Agent configuration

Configuration is loaded from `$ACP_TSP_CONFIG`, or `$XDG_CONFIG_HOME/acp-tsp/config.toml` (default `~/.config/acp-tsp/config.toml`). Missing files use defaults. Per-agent entries replace supplied command/args fields and merge environment variables with preset values:

```toml
default_agent = "claude"

[agents.claude]
command = "npx"
args = ["-y", "@agentclientprotocol/claude-agent-acp@0.87.0"]
env = { EXAMPLE_SETTING = "value" }
```

Adapter-specific session metadata goes under `[agents.NAME.session_meta]` and is sent
as `_meta` on session/new/load/resume/fork. For example, disable Claude's bypass mode:

```toml
[agents.claude.session_meta.claudeCode.options]
allowDangerouslySkipPermissions = false
```

Codex `api-key` authentication reads `CODEX_API_KEY` first, then `OPENAI_API_KEY`
from its process environment; set either in `[agents.codex.env]` or the
environment that launches acp-tsp. The adapter also accepts
`authenticate._meta[\"api-key\"].apiKey`, but this client does not prompt for or
record secrets. `chat-gpt-device-code` uses the URL elicitation dialog.

Both adapters recognize `clientCapabilities._meta.terminal_output = true` and
stream command output in tool-call `_meta.terminal_output` updates (with
`terminal_info` and `terminal_exit`); Claude also recognizes
`clientCapabilities._meta["terminal-auth"] = true`. Steering is opt-in via
`InitializeResponse._meta.steering.supported`: while a turn runs the client
sends `_session/steering` with `{sessionId, prompt, _meta:
{steering: {idleBehavior: "promptRequired"}}}`. Claude returns
`promptRequired` when the turn just ended. Codex 2.1.1 advertises steering but ignores
`idleBehavior`, so a steer racing the end of a turn would start a detached turn: acp-tsp
queues instead of steering for Codex. Without a running turn the client uses ordinary queued
`session/prompt`.

MCP servers may be configured globally or per agent; per-agent entries are appended to the
global list. HTTP and SSE transports require matching agent capabilities; unsupported servers
are skipped with an error shown in the UI.

```toml
[[mcp_servers]]
name = "local-tools"
command = "my-mcp-server"
args = ["--stdio"]
env = { TOKEN = "example" }

[[agents.claude.mcp_servers]]
type = "http" # or "sse"
name = "remote-tools"
url = "https://example.invalid/mcp"
headers = { Authorization = "Bearer example" }
```

When the agent advertises MCP-over-ACP (`mcpCapabilities.acp`), new sessions get an in-process
`tern` MCP server whose `tern_open` tool opens a workspace file in Tern (`tern open`).

Terminal commands requested by the agent inherit the user's privileges. Session roots constrain
the *working directory* only: terminal commands are **not sandboxed** and can access files
outside those roots.

Built-in adapters are pinned: Claude `@agentclientprotocol/claude-agent-acp@0.87.0`, Codex `@agentclientprotocol/codex-acp@2.1.1`.

## agy (Google Antigravity)

The ACP registry's `antigravity-acp` entry installs Google's separate ACP server binary (not
the `agy` CLI). Install it with:

```sh
acp-tsp agents install agy
# Non-interactive install:
acp-tsp agents install agy --yes
acp-tsp agents
acp-tsp agents auth agy --type oauth-personal
```

The installer downloads the current registry release for your OS/architecture, verifies its
SHA-256 when published, and stores the server under `$XDG_DATA_HOME/acp-tsp/agents/agy/`
(default `~/.local/share/acp-tsp/agents/agy/`). It records the registry command and arguments
in a manifest and updates `current`. The binary is proprietary: Google's
[Antigravity FAQ](https://antigravity.google/docs/faq) says use of third-party software to
access Antigravity may violate its Terms of Service. The installer displays this warning and
requires confirmation (or `--yes`).

Choose an auth method with `acp-tsp agents auth agy --type oauth-personal|gemini-api-key|oauth-business|agent-platform`.
`oauth-personal` performs a one-time browser login on the first session. `gemini-api-key` reads
`GEMINI_API_KEY` from the environment. Auth settings are merged into
`~/.gemini/antigravity-acp/settings.json`; an existing file is backed up before replacement.

`acp-tsp agents update` compares registry versions with the pinned Claude/Codex presets;
`--write` records updated adapter package versions as per-agent config overrides.

Host-specific limitation: the registry's agy server `1.3.0` currently exits with `SIGILL`
on this Proxmox LXC host (Intel i5-13600K exposed without AVX-512), including `--help`.
`acp-tsp probe --agent agy` therefore reports failure here. This is a host/binary compatibility
issue, not a general agy requirement.

## Keys

| Key | Action |
|---|---|
| Enter / Alt+Enter / Shift+Enter | send (steer an active turn if advertised) / queue a normal next prompt / newline |
| `/` (empty composer) | search agent slash commands and client actions |
| ↑ ↓, Enter, Escape (picker) | choose, activate, or close the picker |
| `r`, `d` (session picker) | resume; delete after pressing `d` again to confirm |
| Shift+Tab | cycle mode (prefers the agent's mode config option) |
| `/usage` / click composer usage or context meter | open usage dashboard (Overview, Details, Refresh) |
| r / Escape (usage dashboard) | refresh the adapter's local usage report / close |
| Click active configuration chip (for example, Codex Plan) | return that select option to its default |
| ↑ ↓ | prompt history |
| Escape, Ctrl+C | cancel the running turn |
| 1–9, Escape | answer the permission dialog (Escape rejects) |
| @ | pick a workspace file; mentions attach file links, and supported text/images/audio |
| Enter / Escape in an elicitation | accept validated form fields / cancel; URL requests offer Done and Cancel |
| k while a terminal card is focused | kill its running process |
| r | restart an agent that exited |
| Ctrl+D (empty composer), Ctrl+C twice | quit |

During a running turn, Enter steers immediately when the agent advertises steering;
Alt+Enter always queues the next normal prompt. Without steering support both send keys
queue while a turn runs. Steering messages appear in the current turn's transcript.
File mentions stay inside the session workspace roots, including additional directories.
Small text files are embedded only when the agent advertises embedded context;
images and audio are sent only when their respective prompt capabilities are advertised.

The slash picker separates agent commands from `acp-tsp` actions, with agent commands
first; client actions are labeled `acp-tsp ·` so identical names (such as Claude's
`/config`) are distinguishable. Agent commands are inserted into the composer and
sent as ordinary prompt text on Enter, except Claude's `/usage`, which opens the
client usage dashboard. A client command typed or pasted in full
(`/sessions` + Enter) runs directly unless the agent advertises the same name;
the agent command wins (except `/usage`). `/usage` uses Claude's local `/usage`
only while idle, hiding its query turn. Codex limits come from a short-lived
`codex-acp cli app-server` sidecar calling `account/rateLimits/read`, even during
a running turn; it reports windows, credit balance, and available resets.
Refresh with `r`; Details shows Claude's report or a human-readable Codex summary
(without account or reset-credit IDs). Claude does not expose reset credits.
Codex's `/status` remains available as an ordinary agent command.
The dashboard also totals this session's reported tokens, cost, context and turn
durations; no cross-session activity is inferred. The composer usage text and
context meter open it. Codex's `collaboration_mode` Plan setting appears as a
chip while active; click to return to default, use `/plan` or `/config` to enable
it again. Codex asks structured questions (`request_user_input`) in Plan mode; in
Default mode only when its under-development `default_mode_request_user_input`
feature is enabled in Codex's own config (`[features]` in `~/.codex/config.toml`).
Checked with codex 0.159.3 (bundled with codex-acp 2.1.1) and 0.160.0, where it is off
by default.
`/goal` opens a goal-control form only when the agent advertises `_session/goal`
(some adapters expose it only to AIR clients).
The dock account chip opens account details on click; the account e-mail is hidden
until this sheet is opened. Session settings use a native preferences sheet; the
provider picker opens a provider's routing preferences, where saving replaces
its entire headers map. Existing secret headers cannot be read back: enter the
full replacement JSON map before saving. Terminal login methods temporarily
hand the pane back to the terminal, then reconnect.

## Logs and environment

Runtime logs go to `$XDG_STATE_HOME/acp-tsp/acp-tsp.log`, falling back to `~/.local/state/acp-tsp/acp-tsp.log`. `ACP_TSP_LOG` sets the level for acp-tsp (`debug`; default `info`); full directives such as `agent_client_protocol=debug` are taken as written, but dependency debug logs contain raw ACP payloads, including account e-mails.

`ACP_TSP_RECORD=/path/to/file.jsonl` (or `--record-acp FILE`) appends a
timestamped ACP wire transcript, including stderr, for explicit debugging.
`_auth/status_update` params and `email` fields are redacted; stderr is
redacted entirely. **Other ACP payloads include prompts, file contents, and
potentially secrets**: protect the file and review/sanitize before sharing.

- `ACP_TSP_CONFIG`: configuration file path.
- `ACP_TSP_TRACE_WIRE=1`: log every ACP line at `trace` (needs `ACP_TSP_LOG=trace`); payloads may contain account information.
- `TERN_TSP=0`: disables TSP detection/rendering.
- `TERN_TSP_RECORD`: records TSP traffic through `tern-sdk`.
- `ACP_TSP_RECORD`: append redacted ACP JSONL (`t`, `dir`, `line`).

## Development

`cargo test` runs unit and in-process fake-agent tests. `scripts/tern-e2e.sh` drives the release binary inside a headless `tern serve` for manual end-to-end checks against real agents.

Sanitized adapter transcripts under `tests/fixtures/{claude,codex}/` come from
Claude 0.87.0 (Read tool plus session/list) and Codex 2.1.1 (terminal-output
command and read-only plan review); `tests/replay_agents.rs` replays them
through the ACP bridge, model, and view. The `usage.md` and `status.md` fixtures
are captured adapter-local usage reports, with account e-mail, workspace path
and session ID sanitized where present.

See [docs/SUPPORT.md](docs/SUPPORT.md) for what is verified per agent, and [docs/PLAN.md](docs/PLAN.md) for protocol research, scope and planned stages.
