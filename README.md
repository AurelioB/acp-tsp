# acp-tsp

`acp-tsp` is an ACP client that runs in a Tern agent block and renders the conversation natively with Tern Surface Protocol (TSP).

Status:

- Stage 1 (basics) done: prompts, streamed answers/thoughts, tool calls with diffs, plans, permission dialogs, cancel, agent auth, confined `fs/*`, restart.
- Stage 2 (rest of ACP v1 + unstable features) done: `terminal/*`, session list/load/resume/close/delete/fork, `additionalDirectories`, modes and config options, slash commands, file mentions with embedded context/images/audio, MCP servers, form/URL elicitation, terminal auth, logout, usage, `$/cancel_request`, providers, plan operations, notices, compaction, subagents, end-turn usage, MCP-over-ACP (`tern_open` tool). Not implemented: NES (`nes/*`, `document/*`) and draft protocol v2 — see [docs/PLAN.md](docs/PLAN.md) §10 D2.
- Stage 3 (agent-specific extensions) in progress.

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

Use `--load ID` to replay an existing session (falling back to resume if replay is unsupported),
`--resume ID` to resume without replay, or `--continue` to pick the most recently updated session
in the working directory. These flags are mutually exclusive. Repeat `--add-dir DIR` to expose
additional existing workspace directories when the agent supports them.

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
| Enter / Shift+Enter | send / newline |
| `/` (empty composer) | search agent slash commands and client actions |
| ↑ ↓, Enter, Escape (picker) | choose, activate, or close the picker |
| `r`, `d` (session picker) | resume; delete after pressing `d` again to confirm |
| Shift+Tab | cycle mode (prefers the agent's mode config option) |
| ↑ ↓ | prompt history |
| Escape, Ctrl+C | cancel the running turn |
| 1–9, Escape | answer the permission dialog (Escape rejects) |
| @ | pick a workspace file; mentions attach file links, and supported text/images/audio |
| Enter / Escape in an elicitation | accept validated form fields / cancel; URL requests offer Done and Cancel |
| k while a terminal card is focused | kill its running process |
| r | restart an agent that exited |
| Ctrl+D (empty composer), Ctrl+C twice | quit |

Prompts typed during a turn are queued and sent when it ends.
File mentions stay inside the session workspace roots, including additional directories.
Small text files are embedded only when the agent advertises embedded context;
images and audio are sent only when their respective prompt capabilities are advertised.

The slash picker offers `/new`, `/sessions`, `/resume`, `/close`, `/delete`,
`/mode`, `/config`, `/logout`, `/restart`, and `/quit` when supported by the
agent; `/fork` and `/providers` appear when their unstable capabilities are
available. Agent commands are inserted into the composer and sent as ordinary
prompt text on Enter. A client command typed or pasted in full (`/sessions` + Enter) runs
directly unless the agent has a command with the same name (Claude, for example, has its
own `/config`), in which case the agent's command wins. Session settings use a native preferences sheet; the
provider picker opens a provider's routing preferences, where saving replaces
its entire headers map. Existing secret headers cannot be read back: enter the
full replacement JSON map before saving. Terminal login methods temporarily
hand the pane back to the terminal, then reconnect.

## Logs and environment

Runtime logs go to `$XDG_STATE_HOME/acp-tsp/acp-tsp.log`, falling back to `~/.local/state/acp-tsp/acp-tsp.log`. `ACP_TSP_LOG` sets the level for acp-tsp (`debug`; default `info`); full directives such as `agent_client_protocol=debug` are taken as written, but dependency debug logs contain raw ACP payloads, including account e-mails.

- `ACP_TSP_CONFIG`: configuration file path.
- `ACP_TSP_TRACE_WIRE=1`: log every ACP line at `trace` (needs `ACP_TSP_LOG=trace`); payloads may contain account information.
- `TERN_TSP=0`: disables TSP detection/rendering.
- `TERN_TSP_RECORD`: records TSP traffic through `tern-sdk`.

## Development

`cargo test` runs unit and in-process fake-agent tests. `scripts/tern-e2e.sh` drives the release binary inside a headless `tern serve` for manual end-to-end checks against real agents.

See [docs/PLAN.md](docs/PLAN.md) for protocol research, scope and planned stages.
