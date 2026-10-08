# ACP exposure of target agents — research report

Date: 2026-10-08. Scope: how agy (Google Antigravity CLI), OpenAI Codex, and Anthropic Claude Code expose ACP, and exactly which ACP features each supports. Background: Gemini CLI.

## 0. Method & limitations

- All claims cite source URLs or local file paths. Anything not directly documented is marked [INFERENCE].
- **This agent had no shell/exec tool** (tools available were read/find/grep/glob/web_search). Therefore the requested `codex --help` / `claude --help` / `agy --help` could NOT be run. Local versions were read from config files instead:
  - Codex: `0.160.1` — `/home/agent/.codex/version.json` (`{"latest_version":"0.160.1",...}`). Binary `/home/agent/.local/bin/codex` is a 275.7 MB native binary.
  - Claude Code: `2.1.291` — `/home/agent/.claude/.last-update-result.json` (`version_to: "2.1.291", path: "native"`). Binary `/home/agent/.local/bin/claude` is a 238.1 MB native binary.
  - `agy`: not installed on this machine (not in `/home/agent/.local/bin/`).

---

## 1. agy — Google Antigravity CLI

### 1.1 What it is
- `agy` is the terminal surface of Google Antigravity 2.0, powered by the "Gemini 3.5 agent harness", sharing config/auth/agent core with the Antigravity IDE; it descends from Gemini CLI (config under `~/.gemini/antigravity-cli/`). Source: `https://raw.githubusercontent.com/Idun-Group/antigravity-plugin-cc/main/docs/antigravity-cli-reference.md` (verified against `agy --version 1.0.3`, May 2026).
- Official repo: `https://github.com/google-antigravity/antigravity-cli` (README, fetched 2026-10).
- Auth: silent keyring sign-in, else browser OAuth (Google account); over SSH prints a URL+code loop; `/logout` clears. There is **no API-key env var** for the standard preview tier. Source: Idun reference + repo README.
- Flags (v1.0.3, from `agy --help`, per Idun reference): `-p/--print/--prompt`, `--print-timeout <dur>` (default 5m0s), `--dangerously-skip-permissions`, `--sandbox`, `--add-dir <path>`, `-c/--continue`, `--conversation <id>`, `-i/--prompt-interactive`, `--log-file <path>`. Subcommands: `changelog`, `help`, `install`, `plugin`/`plugins`, `update`. **No `--model`/`-m`** flag.

### 1.2 Does agy have an ACP mode? **No — not in the CLI.**
- Open feature request: `google-antigravity/antigravity-cli` issue **#31** "Feature request: add ACP (Agent Client Protocol) stdio JSON-RPC mode" — still **open** (created 2026-05-20, updated 2026-10-06). `https://github.com/google-antigravity/antigravity-cli/issues/31`. The issue states agy v1.0.0 exposes only TUI, `-i`, `-p`, none ACP-drivable.
- A commenter notes `claude --acp` and `codex acp` are actually provided by third parties (Zed/JetBrains), not native — same status applies to agy.
- Google's Antigravity FAQ explicitly discourages third-party access: "Using third party software, tools, or services to access Antigravity is a violation of our Terms of Service" (`https://antigravity.google/docs/faq`), quoted in issue #31.

### 1.3 Official ACP path: a separate `agy_acp_server` binary
- The ACP registry (curated by the ACP project, CI-verified for authMethods) has an entry **`antigravity-acp`** authored by **Google LLC**, license **proprietary**, version **1.3.0**: `https://raw.githubusercontent.com/agentclientprotocol/registry/main/antigravity-acp/agent.json`.
  - Distribution: a standalone executable per platform, not the `agy` CLI:
    - linux-x86_64 / linux-aarch64: `cmd: ./agy_acp_server.par`, **`args: ["--uid="]`**
    - darwin / windows: `agy_acp_server.par` / `agy_acp_server.exe`, no args
    - archives under `https://dl.google.com/agy-extensions/releases/...`
- Launch/auth behavior documented by a third-party ACP client (pi-go), which packaged it (this is community documentation of the server, not Google docs): `https://raw.githubusercontent.com/dimetron/pi-go/main/internal/acp/client/agy/agy.go`:
  - "the `agy` CLI has no ACP mode"; the binary "speaks ACP over stdio, taking `--uid=` on Linux".
  - Search paths: `~/.pi-go/acp/agy/`, `~/.antigravity/acp/`, `~/.local/bin/`, `/usr/local/bin/`, then PATH.
  - **Auth is separate from the agy CLI login:** the server "refuses session/new until an auth method is selected in `~/.gemini/antigravity-acp/settings.json` (`"auth": {"type": "oauth-personal" | "gemini-api-key" | "oauth-business" | "agent-platform"}")", and `oauth-personal` needs a one-time interactive browser login. Binary `.par` on Linux/Windows-less platforms; `.exe` on Windows.
  - Env override used by pi-go: `PI_ACP_AGY_CMD`.
- The exact ACP method surface / capabilities of `agy_acp_server` are **not published in-tree** and I could not obtain the binary. Treat per-feature support as **unknown/[INFERENCE]** below.

### 1.4 Community adapters that wrap agy itself (non-Google)
- `shindgew/agy-acp` (`https://github.com/shindgew/agy-acp`, README): Node/Bun adapter.
  - Architecture: drives one interactive `agy` PTY per ACP session (`agy --prompt-interactive ... --conversation <id> --sandbox ...`); reads steps from agy's conversation SQLite DB (`~/.gemini/antigravity-cli/conversations/<id>.db`), never parses PTY stdout as agent text.
  - Config options map to flags: `mode`→`--mode`, `model`→`--model`, `reasoningEffort`→`--effort`.
  - Cancel: SIGINT then SIGKILL. `--sandbox` on by default; skip-permissions opt-in.
  - Sessions persist under `~/.agy-acp-state` for list/load/resume. **v1**: `session/load` replays history, `session/resume` reattaches without replay. **draft v2**: only `session/resume` (with `replayFrom: {"type":"start"}` to replay).
  - Explicit disclaimer: Google ToS risk; use Vertex/AI Studio keys instead.
- `shubzkothekar/antigravity-acp` (`https://github.com/shubzkothekar/antigravity-acp`, MIT): another Bun ACP server for `agy` with live streaming; cited as a wire-format reference by shindgew.
- A Zed discussion "Add Antigravity CLI to ACP Registry" exists: `https://github.com/zed-industries/zed/discussions/57221`.

### 1.5 agy feature matrix cell summary
- Only reliably knowable ACP path is `agy_acp_server` (registry `antigravity-acp`), plus the unofficial PTY wrappers. The registry entry only asserts authMethods are valid. Everything else (loadSession, promptCapabilities, sessionCapabilities, modes, commands, extensions) is **not documented** by Google and was not obtainable → mark `unknown` with the community-adapter equivalents noted.

---

## 2. Codex

### 2.1 Native ACP? **No.**
- Open request: `openai/codex` issue **#30052** "Built-in ACP protocol support, similar to Gemini CLI ACP mode?" (open, labels CLI/app-server/enhancement): `https://github.com/openai/codex/issues/30052`. Related: issue **#2785** "Support the Agent Client Protocol" (2025-08-27).
- Codex exposes an **App Server** protocol instead; adapters translate ACP → App Server.

### 2.2 Adapters
- **Old:** `zed-industries/codex-acp` — Rust adapter around the Codex CLI. **Archived Jul 22, 2026**; README redirects to `agentclientprotocol/codex-acp`: `https://raw.githubusercontent.com/zed-industries/codex-acp/main/README.md`. npm package `@zed-industries/codex-acp` (0.16.0) is now **deprecated** (`https://www.npmjs.com/package/@zed-industries/codex-acp`, npm.io). Old README features: context @-mentions, images, tool calls+permissions, following, edit review, TODO lists, slash commands `/review`, `/review-branch`, `/review-commit`, `/init`, `/compact`, `/logout`, client MCP servers, auth methods (ChatGPT subscription, `CODEX_API_KEY`, `OPENAI_API_KEY`).
- **Current:** `agentclientprotocol/codex-acp` (TypeScript, "starts the Codex App Server, translates ACP requests into Codex operations, and maps Codex events back"). npm `@agentclientprotocol/codex-acp` (npm shows **1.1.0**; unpkg shows a **2.0.0** README). Sources: `https://raw.githubusercontent.com/agentclientprotocol/codex-acp/main/README.md`, `.../src/index.ts`.

### 2.3 Launch
- `npx -y @agentclientprotocol/codex-acp` (or global `codex-acp`). Bundles a compatible `@openai/codex` dependency.
- Env (README): `CODEX_API_KEY` (precedence over `OPENAI_API_KEY`), `OPENAI_API_KEY`, `CODEX_PATH` (run a different Codex binary), `CODEX_CONFIG` (JSON merged into session config), `MODEL_PROVIDER`, `DEFAULT_AUTH_REQUEST` (ACP auth request JSON), `INITIAL_AGENT_MODE` (`read-only`|`workspace-write`|`agent`|`agent-full-access`), `NO_BROWSER` (hide browser ChatGPT auth), `APP_SERVER_LOGS`.
- Subcommands (`src/index.ts`): `--version`; `login` (run from CLI); `cli` (passthrough to the Codex binary via `runCodexCli(CODEX_PATH, args)`). Otherwise starts the stdio ACP server. It spawns the Codex app-server process; on stdin close it ends stdin and kills the process after 2 s.

### 2.4 Initialize response (`src/CodexAcpServer.ts` ~L426-470)
- `protocolVersion` = ACP PROTOCOL_VERSION (v1). `agentInfo`: name = package name, title `"Codex"`, version.
- `agentCapabilities`:
  - `loadSession: true`
  - `promptCapabilities: { embeddedContext: true, image: true }` (**no `audio`**)
  - `mcpCapabilities: { acp: false, http: true, sse: false }`
  - `sessionCapabilities: { resume, list, close, delete, fork, additionalDirectories, subagents }` (all `{}`)
  - `auth: { logout: {} }`; `providers: {}`
  - `_meta.authStatus` (presence marker: agent pushes `_auth/status_update`)
- Top-level `_meta`: `steering: {supported: true}`; and for AIR clients only `jetbrains.air` with `goal` + capability list.

### 2.5 Methods (`src/index.ts`)
`initialize`; `session/new`, `session/load`, `session/fork`, `session/list`, `session/delete`, `session/resume`, `session/close`, `session/setMode`, `session/setConfigOption`, `session/prompt`, notification `session/cancel`; `authenticate`; `logout`; `providers/list`, `providers/set`, `providers/disable`. Extension requests: `authentication/status`, `authentication/logout`, legacy `_codex/session/set_model`, `_session/steering`, `_session/async_task/stop`, `_session/goal` (goal control). Uses client `session/request_permission` and client `elicitation/create` (URL mode for auth) — see `src/CodexAcpServer.ts` L1243 (`acp.methods.client.elicitation.create`).

### 2.6 Auth methods (`src/CodexAuthMethod.ts`)
- `api-key` ("API Key", `_meta.api-key.provider = openai`)
- `chat-gpt` ("ChatGPT") — omitted when `NO_BROWSER`
- `chat-gpt-device-code` ("ChatGPT (device code)") — only when client supports URL elicitation
- `gateway` ("Custom model gateway", `_meta.gateway.protocol=openai`, `restartRequired=false`) — only when client declares `clientCapabilities.auth._meta.gateway === true`; request carries `baseUrl`, `headers`, optional `providerName`.

### 2.7 Session modes & config options (`src/AgentMode.ts`, `src/ModelConfigOption.ts`)
- Modes: `read-only` ("Read-only"), `workspace-write` ("Workspace access"), `agent` ("Auto review"), `agent-full-access` ("Full access"). `DEFAULT_AGENT_MODE = agent`. Each maps to Codex approval/sandbox policies. Also exposed as config option id `"mode"` (category `mode`).
- Modes also as `session/setMode` values; AIR-only `_meta.jetbrains.air.kind` = `standard`/`auto_review`/`full_access`.
- Config options: `model` (id `model`, category `model`) and `reasoning_effort` (id `reasoning_effort`, category `thought_level`).

### 2.8 Slash commands (`src/CodexCommands.ts` getBuiltinCommands)
`/plan`, `/mcp` (hint for reconnect), `/skills`, `/status`, `/review` (optional instructions), `/review-branch`, `/review-commit`, `/compact`, `/goal` (hint `[<objective>|clear|pause|resume]`), `/rename`, `/logout`. Plus dynamic `$<skill>` commands from `skills/list`. AIR gets `_meta.jetbrains.air.commandAction` on `/plan` and `/goal`, and `skillPath` on `$skill`.

### 2.9 Plans & tool calls
- Plan mode → Markdown plan streamed as `plan_update` when the client declares draft `clientCapabilities.plan` (throttled 150 ms); with `planContentDelta` appended via `_meta...contentDelta`; otherwise full snapshots or `agent_message_chunk`. Completed plan triggers a **plan-review tool call** `plan-review:<id>`, `kind: switch_mode`, options `implement_plan` (allow_once) / `revise_plan` (reject_once). `turn/plan/updated` → standard `plan` with entries. (docs/air-extensions.md).
- Tool call kinds from Codex items (docs/air-extensions.md "Codex items and ACP fields"): `commandExecution` (execute; read/search/listFiles variants; terminal channel; stdin via `_meta.terminal_input`), `fileChange` (edit + `diff` blocks, optional Git patch), `mcpToolCall` (execute, `_meta.is_mcp_tool_call`), `dynamicToolCall`, `collabAgentToolCall`/`subAgentActivity` (subagents), Guardian approval review (`kind: think`), `webSearch` (`kind: search`), `imageGeneration`, `imageView`, MCP elicitation-as-permission, context compaction.
- Permission options map Codex `availableDecisions` → ACP kinds (`accept`→allow_once, `acceptForSession`/`acceptWithExecpolicyAmendment`→allow_always, `decline`/`cancel`→reject_once; unknown/malformed set fails closed with `cancel`). File-change options: allow_once / allow_always / reject_once.

### 2.10 Client-side fs / terminal
- Codex adapter runs shell commands itself (App Server); the ACP `terminal` content type + `_meta.terminal_info`/`terminal_output`/`terminal_output_delta`/`terminal_exit` are **rendering metadata**, not the client `terminal/*` methods. No use of `fs/read_text_file`/`fs/write_text_file` found in `CodexAcpServer.ts` (grep found none). So Codex-adapter needs no client fs or terminal capabilities.

### 2.11 `_meta` and extension keys
- Zed/JetBrains shared: `_meta.terminal_info`, `terminal_output`, `terminal_output_delta`, `terminal_exit`, `terminal_input` (AIR only), `mcp_output_delta`, `is_mcp_tool_call`, `is_mcp_tool_approval`, `steering`, `quota`, `authStatus`.
- `_meta.codex.error` (legacy session failures); legacy `_meta.codex.*` keys were moved/removed into `_meta.jetbrains.air.*` (docs "Removed keys").
- AIR namespace (`_meta.jetbrains.air`): capabilities, goal (`controlMethod: _session/goal`, actions `set|pause|resume|clear`), `diffPatch` (Git patch), `contentDelta`, `permission`, `recommendedValue`, `asyncTasks`, `agentFileChangeReport(Request)`, `sessionFailure`, `subagent`, `contextCompaction`, `fork`, `phase`, `kind`, `commandAction`, `skillPath`, `customAnswer`.
- **No `_zed`-prefixed keys** found (grep for `_zed|zed.` returned nothing).
- Bug fixes noted for every client (docs "Compatibility rule"): unique MCP startup tool-call id, dynamic-tool result in `content`, no output after a tool call ended, terminal status for replayed image generation; no more `rawOutput.formatted_output` duplication.

### 2.12 Unstable / draft features used
- Draft `clientCapabilities.plan`, `clientCapabilities.session.compaction`, ACP subagent RFD (`clientCapabilities.subagents`), native subagent sessions, `session/resume`, `session/list` with cursor, session fork point. Auth URL elicitation. All AIR extensions are "Experimental" (docs header).

### 2.13 Known deviations/bugs from spec
- AIR-only behavior: omit unchanged top-level fields in `tool_call_update`; keep unchanged `_meta`. Non-AIR clients get prior shapes.
- Unknown/inconsistent permission decision set → fail closed with `cancel` (never invents choices).
- Fork point: missing `messageId` target → `invalidParams`; fingerprint regex enforced (`sha256:[0-9a-f]{64}`).
- `session/load`/`resume` cannot recover omitted MCP server names without explicit `mcpServers` (src log "Skipping MCP server recovery...").
- Child subagent "unknown outcome" fallbacks use `failed`, diverging from draft rule (`docs/subagent-sessions.md`).

---

## 3. Claude Code

### 3.1 Native ACP? **No.**
- `claude --acp` does not exist natively; ACP is provided by Zed/JetBrains adapter(s). Evidence: issue comments in `google-antigravity/antigravity-cli#31` (@sbinnee, @jimmyliao) and `openai/codex#2785` context; also community "bridge" projects for Claude Code (`harukitosa/claude-code-acp`, `szhongren/claude-code-acp`, PyPI `claude-code-acp`).
- Local `claude` binary 2.1.291 (native install). Not executed (no shell tool).

### 3.2 Adapters
- **Old:** `@zed-industries/claude-code-acp` (TS, v0.16.2, 0.16.x, deprecated). `https://www.npmjs.com/package/@zed-industries/claude-code-acp`.
- **Current:** `zed-industries/claude-agent-acp` → renamed **`@agentclientprotocol/claude-agent-acp`**. Repo `https://github.com/zed-industries/claude-agent-acp`; `package.json` version **0.87.0**, deps `@agentclientprotocol/sdk 1.7.0`, `@anthropic-ai/claude-agent-sdk 0.3.287`. Builds on the official Claude Agent SDK. Also a Python impl `claude-code-acp` on PyPI.
- `@zed-industries/claude-agent-acp` npm is deprecated with message "renamed to @agentclientprotocol/claude-agent-acp".

### 3.3 Launch (`src/index.ts`)
- `node dist/index.js` / `npx @agentclientprotocol/claude-agent-acp`.
- Flags: `--cli` (pass through to wrapped native `claude` binary; checked first so `--version`/`-v` forwarded), `--version`/`-v` (prints adapter version).
- Env: `CLAUDE_AGENT_LOGS` (log dir), `CLAUDE_AGENT_ACP_EXPERIMENTAL_V2=1` (serve draft ACP v2), `CLAUDE_MODEL_CONFIG` (JSON `{modelOverrides, availableModels}`), plus SDK-passed Bedrock/Vertex env. stdout reserved for ACP; console.* redirected to stderr.
- `src/serve.ts`: default serves ACP v1 only (`v1AgentApp`); with the env flag, `acpProtocolRouter` routes v2 clients to `v2AgentApp` (draft). Uses `ndJsonStream` over stdio.

### 3.4 Initialize response (`src/acp-agent.ts` ~L2593-2750)
- `protocolVersion: 1` (v1 path). `agentInfo`: name = package name, title `"Claude Agent"`, version.
- `agentCapabilities`:
  - `loadSession: true`
  - `promptCapabilities: { image: true, embeddedContext: true }` (**no audio**)
  - `mcpCapabilities: { http: true, sse: true }` (no `acp` key)
  - `sessionCapabilities: { additionalDirectories, close, delete, fork, list, resume, subagents }`
  - `auth: { logout: {} }`; `providers: {}`
  - `_meta.claudeCode.promptQueueing: true`; `_meta.authStatus` marker
- Top-level `_meta`: `steering: {supported: true}`; AIR-only `jetbrains.air` (capabilities incl. `sessionFailure`, `agentFileChangeReport`, `nativeSubagentSessions`, `asyncTasks`, `recommendedValue`, `diffPatch`, `planFile`, plus `goal` with actions `set|clear`).

### 3.5 Methods
`initialize`, `session/new`, `session/load`, `session/resume`, `unstable_forkSession`, `session/list`, `session/setMode`, `session/setConfigOption`, `session/prompt`, `session/cancel`, `authenticate`; providers `unstable_listProviders`, `unstable_setProvider`, `unstable_disableProvider` (wire `providers/list|set|disable`); client `session/request_permission`; `fs/read_text_file` and `fs/write_text_file` (see 3.9); elicitation create/complete. Extension notification `_claude/sdkMessage` via `extNotification`.

### 3.6 Auth methods (`src/acp-agent.ts` ~L2593-2673)
- Terminal-type methods, gated on `clientCapabilities.auth.terminal === true` OR `clientCapabilities._meta["terminal-auth"] === true`:
  - `claude-ai-login` ("Log in with Claude", args `--cli auth login --claudeai`)
  - `console-login` ("Anthropic Console Login", args `--cli auth login --console`)
  - remote-only `claude-login` ("Log in with Claude", args `["--cli"]`) when the browser redirect to localhost won't work
- `_meta["terminal-auth"] = { command, args, label }` attached when the client supports it.
- Gateway / Bedrock auth methods added when the client opts into gateway auth.
- Auth may be hidden (`shouldHideClaudeAuth()`).

### 3.7 Session modes & config options (`src/session-mode.ts`)
- Modes (ids are Claude SDK permission modes): `default` ("Manual"), `acceptEdits` ("Accept edits"), `plan` ("Plan"), `auto` ("Auto", only if model supports), `bypassPermissions` ("Bypass permissions", gated by `allowBypass`; host can disable via `_meta.claudeCode.options.allowDangerouslySkipPermissions: false`). Config option id `"mode"` (category `mode`).
- Auto-model fallback: if a model lacks Auto, fall back to `acceptEdits` and emit a warning notice.
- Other config options: `model` (id `model`), `effort` (category `thought_level`, per docs recommendedValue). Model config via SDK; `CLAUDE_MODEL_CONFIG` overrides unless caller passes `_meta.claudeCode.options.settings`.
- AIR-only `_meta.jetbrains.air.kind`: default/acceptEdits=`standard`, plan=`plan`, auto=`auto_review`, bypassPermissions=`full_access`.

### 3.8 Slash commands (`src/acp-agent.ts` getAvailableSlashCommands ~L10662)
- Built from SDK `supportedCommands()`. Filters CLI terminal-bound commands (from init frame `terminal_slash_commands`, e.g. `/doctor`, `/color`) and static `UNSUPPORTED_COMMANDS = [clear, cost, keybindings-help, login, logout, output-style:new, release-notes, todos]`. MCP prompts renamed `mcp:<name>` (from `... (MCP)`), and always appends an `MCP_AVAILABLE_COMMAND` (`/mcp` status list; adapter runs `/mcp reconnect|enable|disable` via SDK control API, MCP OAuth via URL elicitation). AIR gets skill commands with `_meta.jetbrains.air.kind="skill"` + `skillPath`.
- Live updates: `available_commands_update` on session start and on `commands_changed` (client REPLACES its list).

### 3.9 Plans, tools, client fs
- Plan: `ExitPlanMode` → `kind: switch_mode`, title "Approve Plan" → "Exited Plan Mode". AIR `planFile` mode sends `rawInput.planFilePath` (Claude writes plan under `plansDirectory`, default `~/.claude/plans/`). Without `planFile`, plan text streamed/inline.
- Tool kinds (docs air-extensions.md "Claude tools and ACP fields"): `Bash`/`PowerShell` (execute + terminal), `Read` (read), `Write`/`Edit`/`NotebookEdit` (edit + diff), `Glob`/`Grep` (search), `WebFetch`/`WebSearch` (fetch), `Agent`/`Task` (think/subagent), `ExitPlanMode` (switch_mode), `AskUserQuestion` (form elicitation), `Skill`, `TodoWrite`/`TaskCreate`/`TaskUpdate`/`TaskList`/`TaskGet` (standard `plan`), `ReportFindings`, MCP `mcp__*` (other).
- Client fs: **yes** — `readTextFile` (acp-agent ~L8203) and `writeTextFile` (~L8208) delegate to `this.client` (`methods.client.fs.readTextFile/writeTextFile`, ~L2187/L2191). No client `terminal/*` methods found (grep for `createTerminal|terminal/create|methods.client.terminal|waitForTerminalExit|killTerminal` returned none); Claude runs commands itself and reports output through the `terminal` content channel / `_meta.terminal_*`.
- Terminal channel: client declaring `_meta.terminal_output` gets `content:[{type:"terminal",terminalId}]` + `_meta.terminal_info` + `_meta.terminal_output` + `_meta.terminal_exit`; `terminal_output_delta` appends. Without either, output is a `console` code block in `content`.

### 3.10 `_meta` and extension keys
- Upstream/Zed conventions: `_meta.claudeCode.toolName`, `claudeCode.parentToolUseId`, `claudeCode.toolResponse`, `claudeCode.nonExecutionKind`, `claudeCode.options` (on `session/new`), `claudeCode.mcpServer`; `terminal_output`, `terminal_output_delta`, `terminal-auth`, `is_mcp_tool_call`; `steering`, `quota`, `authStatus`.
- `_claude/*` keys: `_claude/sdkMessage` (extNotification raw SDK messages when enabled), `_claude/model` (usage model), `_claude/origin` (message origin), `_claude/rateLimit` (rate_limit_info). Source: `src/acp-agent.ts` (~L4954, L4140, L6052, L7053).
- `_meta.claudeCode.vertex = {projectId, region}` required for `unstable_setProvider` when `apiType === "vertex"` (~L3184).
- `_meta.claudeCode.options.abortController`, `.additionalDirectories`, `.tools` passed through (~L9420, L9666, L7563).
- AIR namespace: same extension set as Codex plus `planFile`, `customAnswer` (`_meta.jetbrains.air.customAnswer` + root `_meta._askUserQuestionCustomAnswer` because released AIR reads only the root key), and `permission` (with `defaultToNo`).

### 3.11 Permission options (`src/permissions/options/shared.ts`)
Option ids: `allow-once`, `allow-with-updates`, `allow-skill-exact`, `allow-skill-prefix`, `exit-plan-bypass`, `exit-plan-auto`, `exit-plan-accept-edits`, `exit-plan-default`, `exit-plan-clear-auto`, `exit-plan-clear-bypass`, `exit-plan-clear-accept-edits`, `reject`. Grouping by kind: allow_once, then allow_always, then reject kinds. `defaultToNo` hint puts decline first. ExitPlanMode offers one keep-context elevated mode plus (with a non-empty plan) one clear-context choice.

### 3.12 Unstable/draft features
- Draft ACP v2 (opt-in env), `unstable_forkSession`, `unstable_listProviders/setProvider/disableProvider`, session/resume, session/list, ACP compaction RFD, subagent RFD, elicitation, `session.notices`. AIR extensions marked Experimental.

### 3.13 Known deviations/bugs from spec
- Auto-mode model fallback to `acceptEdits` with a warning.
- `bypassPermissions` handled by Claude before `canUseTool`; adapter still treats arriving requests as bypass-immune.
- AIR-only field-omission in `tool_call_update`; non-AIR keeps upstream fields.
- Explicitly unimplemented (docs "Current limits"): Bash output-redirection rewriting, classifier-specific prompt state, ExitPlanMode Ultraplan actions, Computer Use macOS TCC/allowlist dialogs, structured permission lifetime metadata.
- `agentFileChangeReport` uses SDK checkpoint dry-run rewind; may miss Bash/subagent changes → always `declaredComplete: false`.
- Diff-patch preview reads the target file (up to 1 MiB) and may differ in trailing whitespace from what Claude writes.

---

## 4. Gemini CLI (background; agy lineage)

- Native ACP: **yes**, via `gemini --acp` (docs `https://raw.githubusercontent.com/google-gemini/gemini-cli/main/docs/cli/acp-mode.md`). Note Zed issue #43695 and gemini-cli issues #20421/#12042 reference the older `--experimental-acp` flag spelling and auth/429 problems.
- Core methods (docs): `initialize`, `authenticate`, `newSession`, `loadSession`, `prompt`, `cancel`; session control `setSessionMode`, `unstable_setSessionModel`; a **proxied file-system service** (agent reads/writes files through the ACP client). MCP: client registers an MCP server in `initialize`; Gemini connects and exposes its tools.
- Debug: `gemini --acp --debug`; telemetry env `GEMINI_TELEMETRY_ENABLED/TARGET/OUTFILE`.
- Implementation: `packages/cli/src/acp/` (acpStdioTransport, acpRpcDispatcher, acpSessionManager, acpSession, acpUtils, acpErrors, acpCommandHandler, acpFileSystemService); README at `.../packages/cli/src/acp/README.md`. Implements `session/load`/resume (acpResume.test.ts) and `@path` file resolution.
- Relevance: agy descends from Gemini CLI, but the ACP path diverged — agy CLI dropped `--acp`, and Google ships `agy_acp_server` separately. An issue commenter (openab dev) has a stopgap re-targeting `gemini-cli --acp` at Antigravity's backend.

---

## 5. Feature matrix

Legend: **Y**=yes, **P**=partial/conditional, **N**=no, **?**=unknown/undocumented (not verifiable with tools available). For agy the only documented ACP surface is `agy_acp_server` (registry `antigravity-acp`); its internals are unpublished → mostly `?`. Community PTY adapters noted where relevant.

| ACP feature / method | agy (`agy_acp_server` / community) | Codex (`@agentclientprotocol/codex-acp`) | Claude (`@agentclientprotocol/claude-agent-acp`) | Notes & sources |
|---|---|---|---|---|
| Native `--acp` in vendor CLI | N (agy CLI has none; separate server binary) | N | N | agy #31 open; codex #30052 open; claude via adapter only |
| Protocol version | ? (v1 expected) | v1 | v1; draft v2 opt-in (`CLAUDE_AGENT_ACP_EXPERIMENTAL_V2=1`) | codex src/index.ts; claude src/serve.ts |
| `initialize` | Y (implied) | Y | Y | registry CI verifies authMethods |
| `authenticate` | Y (registry CI-verified authMethods) | Y | Y | agy: auth in `~/.gemini/antigravity-acp/settings.json` |
| `logout` / auth.logout | ? | Y (`logout`, `authentication/logout` ext) | Y (`auth.logout`) | codex index.ts; claude init caps |
| `session/new` | Y (required) | Y | Y | pi-go agy.go: server "refuses session/new until an auth method is selected" |
| `session/load` (loadSession) | ? (community: replay) | Y | Y | codex `loadSession:true`; claude `loadSession:true`; shindgew/agy-acp: load replays |
| `session/resume` | ? (community: reattach) | Y | Y | codex sessionCapabilities.resume; claude same; agy community |
| `session/list` | ? (community: persists state) | Y (+`nextCursor`/paging) | Y (paging, `offset:<n>` cursor) | codex/claude sessionCapabilities.list; claude sessionListOffset |
| `session/fork` | ? | Y (`_meta.jetbrains.air.fork` fork point) | Y (`unstable_forkSession`, fork point) | codex SessionFork.ts; claude unlock fork |
| `session/delete` | ? | Y | Y | sessionCapabilities.delete both |
| `session/close` | ? | Y | Y | sessionCapabilities.close both |
| `session/set_mode` (`session/setMode`) | ? (community: `--mode`) | Y | Y | codex AgentMode; claude SessionModeManager |
| `session/set_config_option` | ? (community: mode/model/effort) | Y | Y | codex model+reasoning_effort; claude model+effort |
| `session/prompt` | Y | Y | Y | all |
| `session/cancel` (notification) | ? (community: SIGINT/SIGKILL) | Y | Y | codex index.ts; claude |
| `promptCapabilities.image` | ? | Y | Y | codex+claude init caps |
| `promptCapabilities.audio` | ? | N | N | codex/claude init caps show only image+embeddedContext |
| `promptCapabilities.embeddedContext` | ? | Y | Y | codex/claude init caps |
| Resource links / @-mentions | ? | Y (resource links, @-mentions) | Y (@-mentions) | codex README; claude README |
| `mcpCapabilities.http` | ? | Y | Y | codex init caps http:true; claude http:true |
| `mcpCapabilities.sse` | ? | N | Y | codex sse:false; claude sse:true |
| `mcpCapabilities.acp` | ? | N (false) | absent | codex init caps acp:false |
| `fs/read_text_file` (client method) | ? | N (runs own fs) | Y | claude acp-agent L8203/L2187; codex grep none |
| `fs/write_text_file` (client method) | ? | N | Y | claude acp-agent L8208/L2191 |
| client `terminal/*` methods | ? | N (metadata only) | N (metadata only) | grep found none in either adapter |
| `session/request_permission` (client) | ? (expected) | Y | Y | codex/claude |
| `elicitation/create` (client) | ? | Y (URL, auth/MCP) | Y (form for AskUserQuestion, URL for MCP OAuth) | codex L1243; claude docs |
| Session modes | ? (community: mode) | read-only, workspace-write, agent, agent-full-access | default, acceptEdits, plan, auto, bypassPermissions | AgentMode.ts; session-mode.ts |
| Config options (selectors) | ? (mode/model/effort via community) | model, reasoning_effort, mode | model, effort, mode | as above |
| Slash commands (`available_commands_update`) | ? | /plan,/mcp,/skills,/status,/review,/review-branch,/review-commit,/compact,/goal,/rename,/logout,+$skills | SDK commands filtered (excludes terminal-bound + clear,cost,keybindings-help,login,logout,output-style:new,release-notes,todos), MCP prompts `mcp:*`, +/mcp | CodexCommands.ts; claude getAvailableSlashCommands |
| `agent_message_chunk` | ? | Y | Y | both |
| `agent_thought_chunk` | ? | Y (reasoning) | Y | codex README reasoning; claude |
| `user_message_chunk` | ? | Y | Y | both (replay) |
| `tool_call` / `tool_call_update` | ? | Y | Y | both; kinds diff/execute/read/search/edit/fetch/think/other |
| `diff` content block | ? | Y (+Git-patch `diffPatch` ext) | Y (+Git-patch `diffPatch` ext) | air-extensions docs |
| `terminal` content block / output channel | ? | Y (via `_meta.terminal_*`) | Y (via `_meta.terminal_*`) | air-extensions docs |
| Plan updates (`plan`, `plan_update`) | ? | Y (plan mode; `planContentDelta`) | Y (ExitPlanMode; `planFile` for AIR) | air-extensions docs |
| `available_commands_update` | ? | Y | Y | both |
| `current_mode_update` | ? | Y (implied) | Y | claude SessionModeManager.publishCurrent |
| `usage_update` | ? | Y | Y (`_claude/model`) | claude acp-agent L4140 |
| `session_info_update` | ? | Y | Y | both |
| `compaction_update` | ? | Y (if `session.compaction` declared) | Y (if `session.compaction` declared) | compaction RFD both |
| `notice` session update | ? | Y (if `session.notices`) | Y (if `session.notices`) | air-extensions docs |
| Native subagent sessions | ? | Y (RFD; `sessionCapabilities.subagents`) | Y (RFD) | subagent-sessions.md; claude docs |
| Providers API (`providers/list|set|disable`) | ? | Y | Y (unstable_*) | codex index.ts; claude acp-agent |
| AIR extensions (`_meta.jetbrains.air.*`) | N (not implemented) | Y (opt-in, experimental) | Y (opt-in, experimental) | air-extensions.md both |
| Zed `_meta.terminal_*` conventions | ? | Y | Y | air-extensions docs |
| `_meta.claudeCode.*` / `_claude/*` | N | N | Y | claude only |
| `_zed`-prefixed keys | N | N | N (none found) | grep |
| Goal extension | ? | Y (`_session/goal`, actions set/pause/resume/clear) | Y (`_session/goal`, actions set/clear) | air-extensions docs |
| Async tasks (`_session/async_task/stop`) | ? | Y | Y | air-extensions docs |

### Agent-specific deviations called out in sources
- **agy:** no native ACP; the official server binary's features are undocumented; ToS forbids third-party access to Antigravity accounts (Google FAQ). Community adapters use PTY+SQLite scraping of `agy` (fragile; not a protocol). Suggest bridging `agy_acp_server` (registry `antigravity-acp`) once obtained, but expect a distinct auth setup (`~/.gemini/antigravity-acp/settings.json`) and Linux `--uid=` arg.
- **Codex:** adapter is TS on the Codex App Server; `INITIAL_AGENT_MODE`, `CODEX_CONFIG`, `MODEL_PROVIDER` env; no audio, no SSE, no ACP-MCP; gateway auth is client-opt-in; permission sets fail closed.
- **Claude:** adapter wraps the Claude Agent SDK, not the CLI's own protocol; auth is terminal-launch based; uses client fs read/write; form elicitation for AskUserQuestion; draft v2 opt-in; `_claude/*` extension notifications.

## 6. Open questions / unverified
- Exact `agy_acp_server` capability JSON, method set, modes, commands, and extensions — could not fetch the proprietary binary. [INFERENCE] It likely implements a Gemini-CLI-like surface (initialize/session new/load/prompt/cancel/setSessionMode + fs proxy + MCP), given shared lineage, but this is unverified.
- Local `codex`/`claude` binaries were not executed (no shell tool in this agent), so their `--help` output and any hidden `acp` subcommand were not directly inspected; the "no native ACP" conclusions rest on the cited open issues and community statements.
- codex-acp version numbering is inconsistent across indexes (npm 1.1.0 vs unpkg 2.0.0); verify against the release you install.
