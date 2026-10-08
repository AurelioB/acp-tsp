# ACP (Agent Client Protocol) — Research Report for acp-tsp

All claims are sourced. `[INFERENCE]` marks anything not directly stated by a source. Primary sources: live spec at `agentclientprotocol.com/protocol/v1/*` and `.md` alternates; the `agentclientprotocol/agent-client-protocol` repo (monorepo holding the `agent-client-protocol-schema` crate and `schema/v1/schema.json`); the `agentclientprotocol/rust-sdk` repo (`agent-client-protocol` runtime crate); the ACP Registry repo + CDN index; and docs.rs/lib.rs for legacy versions.

---

## 0. TL;DR / orientation

- ACP is JSON-RPC 2.0 over stdio (default). Client launches agent as subprocess; agent reads stdin, writes stdout; messages are newline-delimited single-line JSON. Agent may log to stderr. Non-message stdout is forbidden. ([transports](https://agentclientprotocol.com/protocol/v1/transports))
- Current **stable wire protocol version is `1`**. Version negotiation is a single integer exchanged in `initialize`; capabilities cover non-breaking additions. ([README](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/README.md), [initialization](https://agentclientprotocol.com/protocol/v1/initialization))
- Property keys are `camelCase`; discriminator string values are `snake_case`; all file paths MUST be absolute; line numbers 1-based. ([overview](https://agentclientprotocol.com/protocol/v1/overview))
- Two message kinds: **methods** (request/response) and **notifications** (no response). ([overview](https://agentclientprotocol.com/protocol/v1/overview))
- Artifact versioning is separate from wire version: `agent-client-protocol-schema` crate = **1.10.2**, runtime crate `agent-client-protocol` = **3.1.0** (2026-10-07), MSRV **1.88**, edition 2024. ([schema Cargo.toml](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/Cargo.toml), [rust-sdk Cargo.toml](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/Cargo.toml), [lib.rs versions](https://lib.rs/crates/agent-client-protocol/versions))

---

## 1. Method catalog

Status legend: **[stable]** = part of stable v1 schema; **[unstable]** = gated in Rust by an `unstable_*` cargo feature and marked `**UNSTABLE**` in schema docs; **[legacy]** = stable but slated for removal.

### 1.1 Client → Agent (requests)

| Method | Gate / capability | Status | Source |
|---|---|---|---|
| `initialize` | baseline (MUST call first) | stable | [schema](https://agentclientprotocol.com/protocol/v1/schema) |
| `authenticate` | agent advertised `authMethods` entry of type `agent` | stable | [authentication](https://agentclientprotocol.com/protocol/v1/authentication) |
| `logout` | `agentCapabilities.auth.logout` present | stable (RFD completed 2026-04) | [authentication](https://agentclientprotocol.com/protocol/v1/authentication), [logout-method RFD](https://agentclientprotocol.com/rfds/logout-method) |
| `session/new` | baseline | stable | [schema](https://agentclientprotocol.com/protocol/v1/schema) |
| `session/prompt` | baseline | stable | [prompt-turn](https://agentclientprotocol.com/protocol/v1/prompt-turn) |
| `session/load` | `agentCapabilities.loadSession` (top-level, not under sessionCapabilities) | stable | [session-setup](https://agentclientprotocol.com/protocol/v1/session-setup) |
| `session/resume` | `agentCapabilities.sessionCapabilities.resume` | stable (RFD 2026-04-22) | [session-setup](https://agentclientprotocol.com/protocol/v1/session-setup) |
| `session/close` | `sessionCapabilities.close` | stable (RFD 2026-04-23) | [session-setup](https://agentclientprotocol.com/protocol/v1/session-setup), [session-close RFD](https://agentclientprotocol.com/rfds/session-close) |
| `session/list` | `sessionCapabilities.list` | stable | [session-list](https://agentclientprotocol.com/protocol/v1/session-list) |
| `session/delete` | `sessionCapabilities.delete` | stable | [session-delete](https://agentclientprotocol.com/protocol/v1/session-delete) |
| `session/set_mode` | agent provided `modes` in session setup | stable but **[legacy]** — "Dedicated session mode methods will be removed in a future version" | [session-modes](https://agentclientprotocol.com/protocol/v1/session-modes) |
| `session/set_config_option` | agent returned `configOptions`; `type:"boolean"` additionally needs client `session.configOptions.boolean` | stable | [session-config-options](https://agentclientprotocol.com/protocol/v1/session-config-options) |
| `session/fork` | `sessionCapabilities.fork` | **[unstable]** (`unstable_session_fork`) | RFD [session-fork](https://agentclientprotocol.com/rfds/session-fork), schema `ForkSessionRequest` `#[cfg(feature = "unstable_session_fork")]` |
| `providers/list` | `agentCapabilities.providers` | **[unstable]** (`unstable_llm_providers`) | `PROVIDERS_LIST_METHOD_NAME` in [agent.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/v1/agent.rs) |
| `providers/set` | `agentCapabilities.providers` | **[unstable]** (`unstable_llm_providers`) | same |
| `providers/disable` | `agentCapabilities.providers` | **[unstable]** (`unstable_llm_providers`) | same |
| `nes/start`, `nes/suggest`, `nes/close` | `agentCapabilities.nes` | **[unstable]** (`unstable_nes`) | [nes.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/v1/nes.rs) |

Note: `authenticate`/`logout` return empty results. `session/new` response carries `sessionId` plus optional `modes` and `configOptions`. `session/load` response may carry `modes`/`configOptions`; `session/resume` likewise. ([session-setup](https://agentclientprotocol.com/protocol/v1/session-setup), [schema](https://agentclientprotocol.com/protocol/v1/schema))

### 1.2 Client → Agent (notifications)

| Notification | Gate | Status | Source |
|---|---|---|---|
| `session/cancel` (params `{sessionId}`) | baseline | stable | [overview](https://agentclientprotocol.com/protocol/v1/overview), [prompt-turn](https://agentclientprotocol.com/protocol/v1/prompt-turn) |
| `nes/accept`, `nes/reject` | `agentCapabilities.nes` | **[unstable]** (`unstable_nes`) | [nes.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/v1/nes.rs) |
| `document/didOpen`, `document/didChange`, `document/didClose`, `document/didSave`, `document/didFocus` | NES negotiation | **[unstable]** (`unstable_nes`) | same |

### 1.3 Agent → Client (requests)

| Method | Gate (client capability) | Status | Source |
|---|---|---|---|
| `session/request_permission` | baseline (clients MUST implement) | stable | [overview](https://agentclientprotocol.com/protocol/v1/overview), [tool-calls](https://agentclientprotocol.com/protocol/v1/tool-calls) |
| `fs/read_text_file` | `clientCapabilities.fs.readTextFile` | stable | [file-system](https://agentclientprotocol.com/protocol/v1/file-system) |
| `fs/write_text_file` | `clientCapabilities.fs.writeTextFile` | stable | [file-system](https://agentclientprotocol.com/protocol/v1/file-system) |
| `terminal/create` | `clientCapabilities.terminal` | stable | [terminals](https://agentclientprotocol.com/protocol/v1/terminals) |
| `terminal/output` | `clientCapabilities.terminal` | stable | same |
| `terminal/wait_for_exit` | `clientCapabilities.terminal` | stable | same |
| `terminal/kill` | `clientCapabilities.terminal` | stable | same |
| `terminal/release` | `clientCapabilities.terminal` | stable | same |
| `elicitation/create` | `clientCapabilities.elicitation.form` and/or `.url` (per-mode) | stable | [elicitation](https://agentclientprotocol.com/protocol/v1/elicitation) |
| `mcp/message` (agent→provider request) | `agentCapabilities.mcpCapabilities.acp` | **[unstable]** (`unstable_mcp_over_acp`) | [mcp-over-acp RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) |
| `_proxy/initialize`, `_proxy/successor` | SDK proxy extension (conductor); successor of a proxy | SDK-provisional, always available in SDK | [protocol.md](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/md/protocol.md) |

### 1.4 Agent → Client (notifications)

| Notification | Notes | Status | Source |
|---|---|---|---|
| `session/update` | Carries a `SessionUpdate` variant (see §2). This is the central streaming channel. | stable | [prompt-turn](https://agentclientprotocol.com/protocol/v1/prompt-turn) |
| `elicitation/complete` | Companion to URL-mode elicitation; carries `elicitationId`. | stable | [elicitation](https://agentclientprotocol.com/protocol/v1/elicitation) |
| `mcp/message` (provider→agent notification) | Carries an inner MCP notification for an active `mcp/message` request. | **[unstable]** | [mcp-over-acp RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) |

### 1.5 Protocol-level (either direction)

| Message | Notes | Status | Source |
|---|---|---|---|
| `$/cancel_request` notification `{requestId}` | Cancels an in-flight request; receiver MUST answer original request (normal result or error `-32800`). Optional to implement. | stable | [cancellation](https://agentclientprotocol.com/protocol/v1/cancellation) |
| `$/...` notifications generally | Implementations MAY ignore unknown `$/` notifications. | stable | [schema CancelRequestNotification](https://agentclientprotocol.com/protocol/v1/schema) |

### 1.6 Extension methods

Any method whose name starts with `_` is reserved for custom extensions, in either direction, as request (with `id`) or notification (without). Unrecognized custom requests → `-32601 Method not found`; unrecognized custom notifications → ignore. ([extensibility](https://agentclientprotocol.com/protocol/v1/extensibility))

---

## 2. `session/update` variants (complete set)

`params.update.sessionUpdate` discriminator (snake_case). From [prompt-turn table](https://agentclientprotocol.com/protocol/v1/prompt-turn) and the `SessionUpdate` enum in [client.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/v1/client.rs):

| `sessionUpdate` | Variant | Payload highlights | Status |
|---|---|---|---|
| `user_message_chunk` | `UserMessageChunk(ContentChunk)` | streamed user message content | stable |
| `agent_message_chunk` | `AgentMessageChunk(ContentChunk)` | streamed agent response | stable |
| `agent_thought_chunk` | `AgentThoughtChunk(ContentChunk)` | streamed reasoning | stable |
| `tool_call` | `ToolCall(ToolCall)` | `toolCallId`, `name?`, `title`, `kind?`, `status`, `content?`, `locations?`, `rawInput?`, `rawOutput?` | stable |
| `tool_call_update` | `ToolCallUpdate(ToolCallUpdate)` | partial update; only `toolCallId` required | stable |
| `plan` | `Plan(Plan)` | `entries[]` with `content`, `priority`(high/medium/low), `status`(pending/in_progress/completed); each update replaces the whole plan | stable |
| `available_commands_update` | `AvailableCommandsUpdate` | `availableCommands[]` | stable |
| `current_mode_update` | `CurrentModeUpdate` | `currentModeId` | stable |
| `config_option_update` | `ConfigOptionUpdate` | full `configOptions[]` | stable |
| `session_info_update` | `SessionInfoUpdate` | optional `title?`, `updatedAt?`, `_meta?` (null clears) | stable |
| `usage_update` | `UsageUpdate` | required `used`, `size`; optional `cost{amount,currency}` | stable (RFD 2026-06-05) |
| `plan_update` | `PlanUpdate` | content update for ID-addressed plan | **[unstable]** `unstable_plan_operations` |
| `plan_removed` | `PlanRemoved` | removal notice | **[unstable]** `unstable_plan_operations` |
| `notice` | `Notice` | advisory, not history; requires client `session.notices` | **[unstable]** `unstable_session_notices` |
| `compaction_update` | `CompactionUpdate` | compaction lifecycle; requires client `session.compaction` | **[unstable]** `unstable_session_compaction` |
| `compaction_summary_chunk` | `CompactionSummaryChunk` | appended summary content | **[unstable]** `unstable_session_compaction` |
| `subagent_update` | `SubagentUpdate` | child session association | **[unstable]** `unstable_subagents` |
| `session_message` | `SessionMessage` | inter-session message upsert | **[unstable]** `unstable_subagents` |
| `session_message_chunk` | `SessionMessageChunk` | inter-session message content | **[unstable]** `unstable_subagents` |

Notes:
- `ContentChunk` now carries an optional `messageId` (agent MAY include; stable, announced 2026). Chunks with same `messageId` belong to one message. ([prompt-turn](https://agentclientprotocol.com/protocol/v1/prompt-turn))
- `ToolCallContent` variants: `content` (a ContentBlock), `diff` (`path`,`oldText?`,`newText`), `terminal` (`terminalId`). ([tool-calls](https://agentclientprotocol.com/protocol/v1/tool-calls), [terminals](https://agentclientprotocol.com/protocol/v1/terminals))
- `ToolKind` values: `read`,`edit`,`delete`,`move`,`search`,`execute`,`think`,`fetch`,`switch_mode`,`other`. `ToolCallStatus`: `pending`,`in_progress`,`completed`,`failed`. ([tool-calls](https://agentclientprotocol.com/protocol/v1/tool-calls))
- `StopReason` (from `session/prompt` response): `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled` (per schema; `PromptResponse.stop_reason`). ([schema StopReason](https://agentclientprotocol.com/protocol/v1/schema))

---

## 3. Capability negotiation

Exchanged in `initialize`. All capabilities are OPTIONAL; omitted/false/null = UNSUPPORTED. New capabilities are non-breaking. ([initialization](https://agentclientprotocol.com/protocol/v1/initialization))

### 3.1 `clientCapabilities` (client → agent, request)

- `fs`: `{ readTextFile: bool, writeTextFile: bool }` (default both false)
- `terminal`: `bool` — gates all `terminal/*`
- `auth`: `{ terminal: bool }` — client can run the agent's configured program interactively for `type:"terminal"` auth methods
- `elicitation`: `{ form?: {}, url?: {} }` — **per-mode opt-in**; omitted/null top-level = unsupported; `{}` advertises *no* modes (differs from MCP). Unknown requested mode → `-32602`.
- `session`: `ClientSessionCapabilities`
  - `configOptions`: `{ boolean?: {} }` — support for `type:"boolean"` config options (empty object = supported)
  - `compaction`: `{}` (unstable_session_compaction)
  - `notices`: `{}` (unstable_session_notices)
- `subagents`: `{}` (unstable_subagents)
- `plan`: `{}` (unstable_plan_operations)
- `nes`, `positionEncodings` (unstable_nes)
Source: schema `ClientCapabilities`/`ClientSessionCapabilities` in [client.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/v1/client.rs), [initialization](https://agentclientprotocol.com/protocol/v1/initialization). Default per schema: `{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false,"auth":{"terminal":false}}`.

### 3.2 `agentCapabilities` (agent → client, response)

- `loadSession`: `bool` (default false) — gates `session/load`
- `promptCapabilities`: `{ image: bool, audio: bool, embeddedContext: bool }` (all default false). Baseline: all agents MUST accept `ContentBlock::Text` and `ContentBlock::ResourceLink`. `image`→Image, `audio`→Audio, `embeddedContext`→Resource.
- `mcpCapabilities`: `{ http: bool, sse: bool }` (both default false; `sse` deprecated by MCP), plus `acp: bool` under `unstable_mcp_over_acp`.
- `sessionCapabilities`: `SessionCapabilities`
  - `list?: {}` — `session/list`
  - `delete?: {}` — `session/delete`
  - `additionalDirectories?: {}` — `additionalDirectories` field on lifecycle requests
  - `resume?: {}` — `session/resume`
  - `close?: {}` — `session/close`
  - `fork?: {}` — `session/fork` **[unstable_session_fork]**
  - Baseline session methods MUST be supported: `session/new`, `session/prompt`, `session/cancel`, `session/update`.
- `auth`: `{ logout?: {} }` — gates `logout`
- `providers?: {}` (unstable_llm_providers), `nes?`, `positionEncoding?` (unstable_nes)
Source: schema `AgentCapabilities`/`SessionCapabilities`/`PromptCapabilities`/`McpCapabilities` in [agent.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/v1/agent.rs), [initialization](https://agentclientprotocol.com/protocol/v1/initialization). Default per schema: `{"loadSession":false,"promptCapabilities":{"image":false,"audio":false,"embeddedContext":false},"mcpCapabilities":{"http":false,"sse":false},"sessionCapabilities":{},"auth":{}}`.

### 3.3 `authMethods[]`

`AuthMethod` is a tagged union on `type`: `agent` (default; client sends its `id` to `authenticate`) or `terminal` (`{type:"terminal", id, name, description?, args?, env?}`). Terminal type only legal when client advertised `auth.terminal`. ([authentication](https://agentclientprotocol.com/protocol/v1/authentication))

### 3.4 Session config options categories

`category` ∈ `mode`, `model`, `model_config`, `thought_level`; categories prefixed `_` are custom. Categories are UX-only, never correctness. `configOptions` array order = priority. ([session-config-options](https://agentclientprotocol.com/protocol/v1/session-config-options))

### 3.5 Additional workspace roots

`additionalDirectories: string[]` on `session/new|load|resume|fork`, gated by `sessionCapabilities.additionalDirectories`; also optionally reported in `SessionInfo`. `cwd` remains primary; relative paths resolve only against `cwd`. ([additional-directories RFD](https://agentclientprotocol.com/rfds/additional-directories), [schema](https://agentclientprotocol.com/protocol/v1/schema))

---

## 4. `_meta` and extension conventions

- `_meta` (type `{ [key: string]: unknown }`) may appear on object types that declare it — many requests/responses/notifications and nested types (content blocks, tool calls, plan entries, capability objects, `SessionInfo`, `SessionInfoUpdate`, compaction, etc.). Not every type has it (e.g. JSON-RPC `Error` does not). ([extensibility](https://agentclientprotocol.com/protocol/v1/extensibility))
- Reserved root-level `_meta` keys for W3C trace context: `traceparent`, `tracestate`, `baggage`. Implementations MUST NOT add custom fields at the root of a spec type (all names reserved). ([extensibility](https://agentclientprotocol.com/protocol/v1/extensibility), meta-propagation RFD)
- Extension **methods** MUST start with `_`; requests carry `id`, notifications omit `id`. Unknown custom request → respond `-32601`; unknown custom notification → ignore. ([extensibility](https://agentclientprotocol.com/protocol/v1/extensibility))
- Custom capabilities SHOULD be advertised under capability objects' `_meta`, e.g. `agentCapabilities._meta."zed.dev".workspace = true`. ([extensibility](https://agentclientprotocol.com/protocol/v1/extensibility))
- Rust SDK serialization note: serde types use `preserve_order` + `raw_value`, so `_meta`/extension payload key order and opaque values are preserved. ([schema Cargo.toml](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/Cargo.toml))
- SDK proxy extension (provisional): `_proxy/initialize` and `_proxy/successor` wrap inner ACP messages for conductor chains. ([protocol.md](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/md/protocol.md))

---

## 5. Errors & cancellation

JSON-RPC 2.0 error object `{code, message, data?}`. `ErrorCode` enum ([error.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/v1/error.rs)):

| Code | Meaning |
|---|---|
| -32700 | ParseError |
| -32600 | InvalidRequest |
| -32601 | MethodNotFound |
| -32602 | InvalidParams |
| -32603 | InternalError |
| -32800 | RequestCancelled (also used for internal cancellation) |
| -32000 | AuthRequired |
| -32002 | ResourceNotFound (data `{uri}`) |
| Other(i32) | any other code preserved |

- `$/cancel_request {requestId}` notification (either side) cancels a request; receiver MUST eventually respond normally or with `-32800`. Best-effort; optional. ([cancellation](https://agentclientprotocol.com/protocol/v1/cancellation))
- Cancelling a prompt turn uses `session/cancel`; the agent then MUST respond to `session/prompt` with `stopReason: cancelled`. Clients MUST answer a pending `session/request_permission` with outcome `cancelled` in that case. ([tool-calls](https://agentclientprotocol.com/protocol/v1/tool-calls), [prompt-turn](https://agentclientprotocol.com/protocol/v1/prompt-turn))
- Nested cancellation: agent can send `$/cancel_request` for its outstanding client requests (e.g. terminal/permission) when a prompt is cancelled; client responds `-32800`. ([cancellation](https://agentclientprotocol.com/protocol/v1/cancellation))

---

## 6. Rust crate — API surface, versions, features

### 6.1 Crate split and versions

Monorepo `agentclientprotocol/agent-client-protocol` publishes the **schema** crate; `agentclientprotocol/rust-sdk` publishes the **runtime** crates. ([README](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/README.md), [rust-sdk README](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/README.md))

- `agent-client-protocol-schema` **1.10.2** — wire types only (v1 + draft v2 modules). Features: `schemars` (default on), `unstable` umbrella, individual `unstable_*`. `unstable_protocol_v2` is deliberately NOT in the `unstable` umbrella. ([Cargo.toml](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/Cargo.toml))
- `agent-client-protocol` **3.1.0** — runtime: roles (`Client`, `Agent`, `Proxy`, `Conductor`), connection builders, handlers, sessions, transports, MCP server attachment. Features (default = none): `process`, `stdio`, `schemars`, `wasm_js`, `unstable` (aggregate), and individual `unstable_end_turn_token_usage`, `unstable_llm_providers`, `unstable_mcp_over_acp`, `unstable_plan_operations`, `unstable_session_compaction`, `unstable_session_fork`, `unstable_session_notices`, `unstable_subagents`, `unstable_protocol_v2`. ([Cargo.toml](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/Cargo.toml), [features.md](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/md/features.md))
- Companion crates: `agent-client-protocol-http`, `-rmcp` (4.0.1), `-conductor`, `-polyfill`, `-trace-viewer`, `-test`, `-cookbook`, `yopo`. MSRV 1.88, edition 2024, resolver 3. ([rust-sdk Cargo.toml](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/Cargo.toml))
- Version history highlights ([lib.rs](https://lib.rs/crates/agent-client-protocol/versions)): 0.0.x→0.4.x (legacy trait/connection API), 0.9–0.15 (many `unstable_*` promotions), 1.0 (2026-06-24), 2.x (Jul–Sep 2026), 3.0 (2026-10-06), 3.1 (2026-10-07).

### 6.2 Current client API (3.x)

Roles: `Client` connects to `Agent`; `Agent` connects to `Client`; `Proxy`↔`Conductor`. ([lib.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/lib.rs), [design.md](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/md/design.md))

Minimal v1 client (from [lib.rs docs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/lib.rs) and [yolo_one_shot_client.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/examples/yolo_one_shot_client.rs)):

```rust
use agent_client_protocol::{AcpAgent, Agent, ConnectionTo};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest, PromptRequest,
    ContentBlock, TextContent, SessionNotification, RequestPermissionRequest,
    RequestPermissionResponse, RequestPermissionOutcome, SelectedPermissionOutcome};

let agent = AcpAgent::from_str("npx -y @agentclientprotocol/codex-acp@latest")?;

agent_client_protocol::Client
    .builder()
    .on_receive_notification(async |n: SessionNotification, _cx| { /* route to TSP */ Ok(()) },
        agent_client_protocol::on_receive_notification!())
    .on_receive_request(async |r: RequestPermissionRequest, responder, _cx| {
        responder.respond(RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id))));
        Ok(())
    }, agent_client_protocol::on_receive_request!())
    .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
        connection.send_request(InitializeRequest::new(ProtocolVersion::V1)).block_task().await?;
        let s = connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
        connection.send_request(PromptRequest::new(s.session_id,
            vec![ContentBlock::Text(TextContent::new("hi"))])).block_task().await?;
        Ok(())
    })
    .await?;
```

Key API pieces:
- `Client.builder()` → `Builder`; `.on_receive_request`, `.on_receive_notification`, `.on_receive_dispatch`, `.on_close`, `.name(...)`.
- `connect_with(transport, closure)` = **active mode** (used by clients; closure drives interaction, then closes). `connect_to(transport)` = **reactive mode** (used by agents/proxies; runs until clean incoming EOF). ([design.md](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/md/design.md))
- `ConnectionTo<Agent>`: `send_request(...).block_task()/.run_until(...)`, `build_session(cwd)`, `build_session_cwd()`, `load_session(...)`, `resume_session(...)`, `add_dynamic_handler`, `incoming_closed()`. Session helpers return `SessionBuilder`/`RestoreSessionBuilder`, with `ActiveSession` giving `send_prompt`/`read_to_string`. ([session.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/session.rs))
- Transports: `Channel` (in-memory), `Lines` (newline-delimited), `ByteStreams` (`AsyncRead`+`AsyncWrite` + embedder drives the future), `Stdio` (native, feature `stdio`), and `AcpAgent` (feature `process`) which itself implements `ConnectTo`. Weirdness to note: `ToolCallUpdate` etc. use the `MaybeUndefined`/`IntoOption` helpers; `IntoMaybeUndefined` controls omit-vs-null.
- Handler ordering is sequential; a handler runs to completion before next message. Long work should use `connection.spawn()` (concurrent with dispatch loop) — don't block the loop. ([design.md](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/md/design.md))
- Incoming EOF is a liveness boundary: pending requests fail with `incoming_transport_closed`; `is_incoming_transport_closed()` detects it. ([design.md](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/md/design.md))

### 6.3 Spawning an agent subprocess

`AcpAgent` (feature `process`) + `AcpAgentConfig { command, args, env }`, constructed via `AcpAgent::new(config)`, `AcpAgent::from_str("npx ...")`, or `from_str` JSON. Presets: `AcpAgent::claude_agent()` (`npx -y @agentclientprotocol/claude-agent-acp@latest`), `AcpAgent::codex()` (`npx -y @agentclientprotocol/codex-acp@latest`). `spawn_process()` is the low-level escape hatch returning stdin/stdout/stderr + child. ([acp_agent.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/acp_agent.rs))

Platform/runtime details that matter for a bridge:
- On Unix the child is put in its **own process group** (`process_group(0)`) so dropping the connection kills the whole tree — important because agents are usually launched behind `npx`/`uvx` wrappers. On Windows `CREATE_NO_WINDOW`. ([acp_agent.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/acp_agent.rs))
- stderr capture is bounded (64 KiB, line truncation), shutdown grace period 1s. ([acp_agent.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/acp_agent.rs))
- `with_debug(callback: Fn(&str, LineDirection))` observes raw lines. ([acp_agent.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/acp_agent.rs))

### 6.4 Send/Sync, runtime, LocalSet — critical for the bridge

- **Current 3.x**: `Role: Debug + Clone + Send + Sync + 'static` ([role.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/role.rs)); the SDK is runtime-agnostic and stores boxed `Send` futures (`BoxFuture`). No `LocalSet` requirement. Examples use `#[tokio::main]` (multithread-capable), though the crate itself does not require Tokio for the generic transports. ([lib.rs](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol/src/lib.rs))
- **Legacy 0.x–2.x** used a different model: `Client`/`Agent` **traits** implemented with `#[async_trait::async_trait(?Send)]` (i.e. **!Send futures**), plus `ClientSideConnection::new(client, outgoing_bytes, incoming_bytes, spawn)` where `spawn: impl Fn(LocalBoxFuture<'static, ()>)`, and `AgentSideConnection::new(...)`. Both connections provided the complementary trait's methods (`conn.initialize(...)`, `conn.new_session(...)`, `conn.prompt(...)`). Because futures were !Send, examples ran inside `tokio::task::LocalSet` with `tokio::task::spawn_local`, and `#[tokio::main(flavor = "current_thread")]`. ([docs.rs 0.4.4 ClientSideConnection](https://docs.rs/agent-client-protocol/0.4.4/agent_client_protocol/struct.ClientSideConnection.html), [legacy example client.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/v0.4.4/rust/examples/client.rs), [docs.rs 0.4.4 Client trait](https://docs.rs/agent-client-protocol/0.4.4/agent_client_protocol/trait.Client.html))
- Practical consequence: choose 3.x for acp-tsp (Send, no LocalSet, `process` feature launches agents, exported `Client` role). The trait-based `Agent`/`Client` API still shown on `agentclientprotocol.com/libraries/rust` describes the older generation; that doc page links `trait.Agent`/`trait.Client` (stale relative to 3.1.0). [INFERENCE] If pinned to a 0.x/2.x crate you must run the bridge on a `LocalSet`/single-thread runtime with a `spawn_local` shim.
- WebAssembly: core + transports compile for `wasm32-wasip1/wasip2`; `wasm_js` for WebCrypto UUIDs on `wasm32-unknown-unknown`; `process`/`stdio` are excluded on WASM. ([features.md](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/md/features.md))

### 6.5 v2 (draft)

Draft v2 exists in the schema behind `unstable_protocol_v2` (parallel `v2` module; `ProtocolVersion::V2`; `Proxy.v2()`, `V2SessionBuilder`, `V2ForkSessionBuilder`). `terminal`/`fs` become client capabilities under a different shape, `mcp` becomes `capabilities.session.mcp.acp`, etc. Not needed for stage-1. ([README](https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/README.md), [mcp-over-acp RFD](https://agentclientprotocol.com/rfds/mcp-over-acp), [version.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/version.rs))

---

## 7. Registry / agent discovery & launch

- **ACP Registry** is a curated list of ACP agents that support authentication; index published at `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json` (plus JetBrains-specific and JetBrains-preview variants, both drop-in replacements). Versions auto-bumped hourly from npm/PyPI/GitHub releases. ([registry README](https://raw.githubusercontent.com/agentclientprotocol/registry/main/README.md), [registry page](https://agentclientprotocol.com/get-started/registry))
- **Distribution types** ([FORMAT.md](https://raw.githubusercontent.com/agentclientprotocol/registry/main/FORMAT.md)):
  - `npx`: `npx <package> [args]` (npm)
  - `uvx`: `uvx <package> [args]` (PyPI)
  - `binary`: per-platform `{archive, cmd, args?, env?, sha256?}`; archive formats `.zip`/`.tar.gz`/`.tgz`/`.tar.bz2`/`.tbz2`/raw only; platform keys `darwin-aarch64|x86_64`, `linux-aarch64|x86_64`, `windows-aarch64|x86_64`.
  - Optional `preview` block (stripped from published stable/JetBrains indexes; served in the JetBrains-preview index).
- **Target agents in the registry** ([live registry.json](https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json)):
  - **Google Antigravity** (`antigravity-acp`, v1.3.0) — *binary only*, proprietary. macOS: `./agy_acp_server.par`; Linux: `./agy_acp_server.par --uid=` (note empty `--uid=` arg placeholder); Windows: `./agy_acp_server.exe`. Archives under `https://dl.google.com/agy-extensions/releases/...`. This is the "agy" agent.
  - **Codex** (`codex-acp`, v2.1.1) — `npx @agentclientprotocol/codex-acp@2.1.1`. (SDK preset: `npx -y @agentclientprotocol/codex-acp@latest`.)
  - **Claude Agent** (`claude-acp`, v0.87.0) — `npx @agentclientprotocol/claude-agent-acp@0.87.0`. (SDK preset: `npx -y @agentclientprotocol/claude-agent-acp@latest`.)
  - Other notable: Cline (`cline@… --acp`), Cursor, Devin, Gemini CLI, GitHub Copilot (public preview), Goose, OpenCode, Qwen Code, Kimi CLI, etc. Full list on [agents page](https://agentclientprotocol.com/get-started/agents).
- Launch convention: registry entries are *launch specs*, not a runtime API. The bridge reads/embeds a spec and spawns the process with stdio piped, then speaks ACP. There is no in-protocol "download/install agent" method. [INFERENCE] For acp-tsp, default `cwd` = workspace root; the bridge should surface the registry as a preset list and store per-agent launch config.
- Agent authentication is a registry admission requirement (CI verifies agents return valid `authMethods`). ([registry README](https://raw.githubusercontent.com/agentclientprotocol/registry/main/README.md))

---

## 8. Unstable / draft RFDs relevant to later stages

Completed & stabilized (now in v1): session list, delete, resume, close, logout, additional directories, session info update, session config options, boolean config options, request cancellation, tool-call name, elicitation, model-config category, message IDs, session usage. ([llms.txt announcements](https://agentclientprotocol.com/llms.txt))

Still draft/unstable (schema behind `unstable_*`):
- `session/fork` (`unstable_session_fork`) — [RFD](https://agentclientprotocol.com/rfds/session-fork)
- MCP-over-ACP `mcp/message` (`unstable_mcp_over_acp`) — [RFD](https://agentclientprotocol.com/rfds/mcp-over-acp)
- Configurable LLM providers `providers/list|set|disable` (`unstable_llm_providers`)
- Plan operations `plan_update`/`plan_removed` (`unstable_plan_operations`)
- Session compaction (`unstable_session_compaction`), session notices (`unstable_session_notices`)
- Subagent sessions (`unstable_subagents`)
- End-turn token usage (`unstable_end_turn_token_usage`)
- NES / next-edit-suggestions (`unstable_nes`)
- Draft protocol v2 (`unstable_protocol_v2`)
- Not yet in the Rust schema: streamable HTTP/WebSocket transport, session usage/compaction RFDs beyond what is listed above. ([RFDs index](https://agentclientprotocol.com/llms.txt))

---

## 9. Gotchas for the bridge

1. `session/load` capability is top-level `agentCapabilities.loadSession`, NOT under `sessionCapabilities` (the spec explicitly notes this will be unified later). ([initialization](https://agentclientprotocol.com/protocol/v1/initialization))
2. Omitted vs explicit-null vs empty-object carry distinct meanings for many caps (e.g. `sessionCapabilities.delete` omitted = unsupported, `{}` = supported; elicitation `{}` = no modes). Handle presence/absence, not just truthiness. ([session-delete](https://agentclientprotocol.com/protocol/v1/session-delete), [elicitation](https://agentclientprotocol.com/protocol/v1/elicitation))
3. The Rust schema is heavily `#[non_exhaustive]` and uses `DefaultOnError`/`VecSkipError`: unknown fields are ignored and malformed optional fields default rather than failing, but discriminator mismatches still fail. Match handlers on typed enums. ([client.rs](https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/agent-client-protocol-schema/src/v1/client.rs))
4. Tool-call `name` is informational only — never a capability or authorization. ([tool-calls](https://agentclientprotocol.com/protocol/v1/tool-calls))
5. `session/prompt` is a single long-lived request whose progress arrives as `session/update` notifications; the bridge must multiplex those to TSP while the request is in flight, and must keep accepting tool-call updates after `session/cancel`. ([prompt-turn](https://agentclientprotocol.com/protocol/v1/prompt-turn))
6. Paths must be absolute; the bridge (as client) owns path authorization/sandboxing for `fs/*` and must enforce the session's root set (`cwd` + `additionalDirectories`). ([additional-directories RFD](https://agentclientprotocol.com/rfds/additional-directories))
7. stdio framing is strictly one JSON-RPC message per line, no embedded newlines; agent stdout must be pure ACP. ([transports](https://agentclientprotocol.com/protocol/v1/transports))
