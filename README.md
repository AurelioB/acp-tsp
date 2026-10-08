# acp-tsp

`acp-tsp` is an ACP client that runs in a Tern agent block and renders the conversation natively with Tern Surface Protocol (TSP). Stage 1 is in progress.

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

## Logs and environment

Runtime logs go to `$XDG_STATE_HOME/acp-tsp/acp-tsp.log`, falling back to `~/.local/state/acp-tsp/acp-tsp.log`. `ACP_TSP_LOG` sets the tracing filter (default `info`).

- `ACP_TSP_CONFIG`: configuration file path.
- `ACP_TSP_TRACE_WIRE`: reserved for opt-in wire tracing; wire payloads may contain account information.
- `TERN_TSP=0`: disables TSP detection/rendering.
- `TERN_TSP_RECORD`: records TSP traffic through `tern-sdk`.

See [docs/PLAN.md](docs/PLAN.md) for protocol research, scope and planned stages.
