# acp-tsp

`acp-tsp` is an ACP client that runs in a Tern agent block and renders the conversation natively with Tern Surface Protocol (TSP).

Status: Stage 1 (basics) done: prompts, streamed answers/thoughts, tool calls with diffs, plans, permission dialogs, cancel, agent auth, confined `fs/*`, restart. Stages 2–3 are in progress; see [docs/PLAN.md](docs/PLAN.md).

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

Built-in adapters are pinned: Claude `@agentclientprotocol/claude-agent-acp@0.87.0`, Codex `@agentclientprotocol/codex-acp@2.1.1`. The agy ACP server is not bundled; installation support is planned for Stage 3.

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
| r | restart an agent that exited |
| Ctrl+D (empty composer), Ctrl+C twice | quit |

Prompts typed during a turn are queued and sent when it ends.

The slash picker offers `/new`, `/sessions`, `/resume`, `/close`, `/delete`,
`/mode`, `/config`, `/logout`, `/restart`, and `/quit` when supported by the
agent; `/fork` and `/providers` appear when their unstable capabilities are
available. Agent commands are inserted into the composer and sent as ordinary
prompt text on Enter. Session settings use a native preferences sheet; terminal
login methods temporarily hand the pane back to the terminal, then reconnect.

## Logs and environment

Runtime logs go to `$XDG_STATE_HOME/acp-tsp/acp-tsp.log`, falling back to `~/.local/state/acp-tsp/acp-tsp.log`. `ACP_TSP_LOG` sets the level for acp-tsp (`debug`; default `info`); full directives such as `agent_client_protocol=debug` are taken as written, but dependency debug logs contain raw ACP payloads, including account e-mails.

- `ACP_TSP_CONFIG`: configuration file path.
- `ACP_TSP_TRACE_WIRE=1`: log every ACP line at `trace` (needs `ACP_TSP_LOG=trace`); payloads may contain account information.
- `TERN_TSP=0`: disables TSP detection/rendering.
- `TERN_TSP_RECORD`: records TSP traffic through `tern-sdk`.

## Development

`cargo test` runs unit and in-process fake-agent tests. `scripts/tern-e2e.sh` drives the release binary inside a headless `tern serve` for manual end-to-end checks against real agents.

See [docs/PLAN.md](docs/PLAN.md) for protocol research, scope and planned stages.
