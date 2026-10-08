# TSP (Tern Surface Protocol) + Rust tern-sdk — exhaustive research

Research date: 2026-10-08. All claims cite the doc page or source file they come from. `[INFERENCE]` marks reasoning I did rather than something stated. This report is research only; no code was written.

---

## 0. Sources

- Protocol docs (crawl root): https://docs.stencil.so/tern/protocol/index.html (markdown alternates used: `.../index.md`). Subpages: transport, handshake, surfaces, documents, input, theme, operations, sdks.
- Element vocabulary: https://docs.stencil.so/tern/elements/{index,layout,text,data,lists,chrome,motion,input,work,el}.htm(l).
- SDK repo: https://github.com/stencil-hq/tern-sdk (Rust crate under `rust/`), raw files at `https://raw.githubusercontent.com/stencil-hq/tern-sdk/main/...`.
- Package: https://crates.io/crates/tern-sdk (0.1.0, published 2026-10-05, MIT).
- ACP v1 (for gap analysis): https://agentclientprotocol.com/protocol/v1/overview and `.../v1/schema`.

---

## 1. What TSP is (and is not)

From `protocol/index.md`:

> "The Tern Surface Protocol (TSP) lets a program in a terminal pane describe its UI as a tree of semantic components. Tern lays that tree out, draws it and animates it natively. A program that speaks TSP stops painting cells... It sends *what* is on screen... and Tern decides *how* it looks."

Ownership split (same page):

| Concern | Owner |
|---|---|
| What is shown | The program (document of nodes from the element vocabulary, changed by frames of ops) |
| Layout | Tern (program never measures text; gets `cols`, `cell` for `rows`/ANSI width only) |
| Animation | Tern (motion kinds clocked by Tern, honors Reduce Motion; program sends no per-frame updates) |
| State and input | The program (keys arrive as pty input; pointer actions come back as events) |
| View-only state | Tern (hover, scroll position never reported; folds and `el` checkbox/radio flips reported via `toggle`/`change`) |
| Theme | Program by default (program asserts its palette; user setting can switch back) |
| History | The program (nodes stay id-addressable in scrollback unless dropped under retention, a `gone` event) |

Key structural fact: **TSP is not a subprocess protocol and has no framing over a separate channel.** It "travels in-band on the pane's pty as APC strings" (`transport.md`), so it works over ssh and through Tern's session daemon. A terminal that does not speak TSP never answers the handshake and the program keeps its own ANSI renderer.

---

## 2. Transport

### 2.1 Framing (`transport.md`)

```
ESC _ tsp ; <verb> [; <key>=<value>]* ; <body> ESC \
```

- `verb` is one letter, case-sensitive.
- A segment after the verb is a **parameter** only if it has the form `key=value` (key `[A-Za-z0-9_-]+`, value printable ASCII other than `;`, no spaces) *and* another `;` follows it. The first segment that isn't starts the body, so a JSON body containing `;` or `=` stays whole. A no-parameter message is `tsp;<verb>;<body>`.
- `body` is UTF-8 JSON, or base64 for `b`. JSON escapes control chars so a body never contains ESC/BEL; an 8-bit `0x9C` inside the string is payload, not a terminator. Invalid JSON body -> `{"ev":"error","msg":"malformed f body: …"}`.
- Terminate with the 7-bit ST `ESC \`. Tern's parser also ends any string at BEL (as kitty does) but TSP defines only ST: **never send BEL-terminated TSP**.
- Unknown verbs and unknown JSON fields are ignored in both directions (unknown verb gets no `error`). New fields are optional forever; an incompatible change bumps the protocol version.

Params Tern reads: `c`, `m` (any verb; chunking), `id`,`mime` (`b` blobs), `key`,`buf`,`seeded` (`o`, reserved for session-daemon snapshots; ignored on a program's `o`).

Verbs:

| Verb | Direction | Meaning |
|---|---|---|
| `q` | program -> terminal | A query (`hello`, blob presence) |
| `o` | program -> terminal | Open a surface |
| `f` | program -> terminal | A frame: an atomic batch of document ops |
| `b` | program -> terminal | A blob |
| `t` | program -> terminal | The program palette |
| `s` | program -> terminal | A stylesheet |
| `x` | program -> terminal | Close a surface |
| `r` | terminal -> program | The reply to a `q` |
| `e` | terminal -> program | An event |

Terminal -> program messages travel on the pty's **input** side, one `ESC _ tsp;<verb>;<json> ESC \` each, **never chunked** (read with an escape buffer that takes whole APC strings of any length and routes `ESC _ tsp;` to TSP before key handling; never echo/print them).

### 2.2 Chunking

Bodies larger than the negotiated `apc` limit (65536 default) are split across messages with the same verb, each with `c=<chunk-id>` (token you choose) and `m=1` on every chunk but the last. Tern joins bodies **byte by byte** before parsing, so a split may fall inside a UTF-8 char but **not where the next chunk's body would start with a parameter-shaped segment**. The joined message keeps the first chunk's other parameters. Rules:

- Send a message's chunks back to back; Tern joins one sequence at a time.
- A sequence is dropped with `error` (`chunked f message c=k7 dropped: interrupted`) when any non-next-chunk TSP message arrives, after 1 s without a chunk (`timed out`), or when the joined body would exceed 24 MiB (`over 25165824 bytes`). RIS drops silently.
- A chunk body must not itself start with a parameter-shaped segment (a body starting `x=1;y…` silently loses `x=1`). Safe split points: right before a `;`, or before any byte other than `A-Z a-z 0-9 _ -`. Base64 blob chunks are safe.
- Tern's parser abandons an APC string longer than **262144 bytes** (from `ESC _` inclusive of params) with no `error`, and the remainder shows as text.

### 2.3 Blobs

```
ESC _ tsp;b;id=<sha256 hex>;mime=image/png;<base64> ESC \
```

- Standard base64 (`+/`, `=`, no line breaks). `id` is hex SHA-256 of decoded bytes (64 digits); references are the lowercase form. Tern rejects (with `error`) a blob without a valid `id`, bad base64, or bytes not matching `id`. `mime` optional. Max 16 MiB decoded. Content-addressed, shared by the pane's surfaces, kept on disk. Ask which are still needed with the `blobs` query.

### 2.4 Windows (`transport.md#windows`)

Through a ConPTY, terminal -> program messages are wrapped as OSC 877:

```
ESC ] 877 ; tsp ; <verb> [; <key>=<value>]* ; <body> ESC \
```

body byte-identical except U+0080–U+009F are sent as JSON escapes `\u0080`…`\u009f` (because U+009C would end the string). Accept both framings and a BEL-terminated OSC 877. Program -> terminal stays APC; requires the bundled `conpty.dll`/`OpenConsole.exe` next to `tern.exe`. The SDK input parser handles APC or OSC 877 (`input.rs`).

---

## 3. Handshake, versioning, auth

### 3.1 Detection (`handshake.md`)

```
ESC _ tsp;q;{"q":"hello","v":[1],"app":"omp","ver":"14.2.0","features":["edit","undo","send"]} ESC \  ESC [ c
```

Program writes `hello` **followed by a DA1 request** (`ESC [ c`) which every terminal answers. A TSP terminal answers the query before the DA1 reply. A DA1 reply with no `tsp;r` before it means no TSP: keep your own renderer. Fields:

- `v`: protocol versions you speak. Tern speaks only `1`; a query whose list lacks it gets **no reply at all** (reads as no TSP).
- `app`: program name; while a surface is open and listening, Tern shows that program as the pane's (tab/palette icon).
- `ver`: your version; Tern doesn't use it.
- `features`: what you handle beyond v1; unknown names ignored.
- Each `hello` replaces the last (a later `hello` without `app`/`features` clears them). Tern forgets them when the program is gone (next shell prompt, process exit, terminal reset) and applies them only while a surface is open+listening.

Program features: `edit` (apply `edit` events, so Tern may keep a native selection), `undo` (apply `undo` events), `send` (submit text supplied in a `send` event through the composer's normal path; a capability, target must also report `sendable:true`).

### 3.2 Reply

```json
{"r":"hello","v":1,"term":"tern","ver":"0.4.3",
 "kinds":["col","row","card","section","rule","spacer","text","md","code","diff","ansi","math",
  "image","kv","table","tree","badge","kbd","icon","spinner","shimmer","elapsed","progress","rate",
  "list","item","tabs","editor","input","status","seg","overlay","toast","rows","picker","prefs",
  "tool","checklist","agent","chart","meter","block","effort","el"],
 "features":["blobs","settle","adopt","dock","program-palette","reduce-motion","aside","scroll","styles","flow"],
 "apc":65536,"credits":2,
 "cols":120,"cell":{"w":8,"h":17},"dark":true,"reduceMotion":false,"hour12":false}
```

| Field | Meaning |
|---|---|
| `v` | version picked from your list (`1`) |
| `term`,`ver` | terminal and version (`tern`, == `TERM_PROGRAM_VERSION`) |
| `kinds` | every node kind it draws; fall back per kind (unlisted kind -> another description, or pre-rendered `rows`) |
| `features` | behavior a kind alone doesn't imply |
| `apc` | largest body per message (65536); chunk larger |
| `credits` | unacknowledged frames allowed (2) |
| `cols`,`cell` | pane width in columns, cell size in pixels; `resize` events update |
| `dark` | dark appearance (`theme` events update) |
| `reduceMotion` | Reduce Motion on (`motion` events update) |
| `hour12` | user's system 12h vs 24h clock |

Terminal features: `blobs`, `settle`, `adopt`, `dock`, `program-palette`, `reduce-motion`, `aside`, `scroll`, `styles`, `flow`.

Note the reply array in the doc example lists 41 kinds while the SDK's `kind::ALL` has 44 (it also names `math` in the kind list — the doc reply example above includes `math`; the SDK list additionally has `section`? verify: SDK ALL = col,row,card,section,rule,spacer,text,md,code,diff,ansi,rows,math,kv,table,tree,badge,kbd,icon,image,list,item,tabs,picker,spinner,shimmer,elapsed,rate,progress,meter,chart,effort,editor,input,status,seg,toast,overlay,tool,agent,checklist,block,prefs,el = 44). `[INFERENCE]` The doc example simply omits some (e.g. `math`, `section` appear in the doc list too); treat `hello.kinds` as authoritative at runtime and degrade per kind.

### 3.3 Other queries

`blobs` asks which blobs Tern already has:

```
-> ESC _ tsp;q;{"q":"blobs","ids":["9f86d0…","2c26b4…"]} ESC \
<- ESC _ tsp;r;{"r":"blobs","have":["9f86d0…"]} ESC \
```

`have` lists held ids in the order asked; asking counts as a use (LRU eviction). Tern answers no other query; unknown `q` gets no reply. Invalid JSON body gets an `error`.

### 3.4 Starting without waiting

When `TERM_PROGRAM=tern` and not inside a multiplexer, the program may send `o` and its first `f` at once assuming v1 capabilities (every kind, apc 65536, credits 2, cols from terminal size, reduceMotion/hour12 false), switching the tty to raw before the query. The reply still decides when it arrives (kinds/apc/credits/appearance replace assumptions). If DA1 answers first or nothing arrives within 1 s, close the surface (`x` with `keep:false`) and use your own renderer. **omp starts this way.**

### 3.5 Environment and opt-outs

- Tern sets `TERM_PROGRAM=tern` and `TERM_PROGRAM_VERSION` for every pane (window, session daemon, daemon reached over `tern remote`). Environment doesn't survive ssh; the query does.
- Tern's "Native program surfaces" setting turns TSP off entirely (never answers `hello`, ignores every TSP message).
- tmux, screen and zellij swallow APC strings — don't probe inside them.
- Programs should offer their own switch to force ANSI (omp has `PI_TUI_NATIVE=0`; SDK honors `TERN_TSP=0`, `TMUX`/`STY`/`ZELLIJ`).

### 3.6 Auth and identity

**There is no authentication or authorization in TSP.** Identity is `app`/`ver` in `hello` plus the `o.role` (CSS `data-surface`) and the `hello.app` (CSS `data-app`). Sandboxing is structural: "No scripts and no raw HTML", program CSS confined to its own surface, no `url()`/`position:fixed`/`@font-face`, `file://` links limited to paths, "nothing runs a command" (`operations.md#security-and-limits`). `[INFERENCE]` A bridge therefore cannot ask Tern to perform privileged host actions; it must do filesystem/terminal work itself.

---

## 4. Client vs server, registration, configuration

### 4.1 Who is what

- **Tern is the terminal/UI host.** It answers `hello`, renders the node tree, sends events. It is "server" only for the one query reply channel and for owning pane screen state.
- **The program is the TSP client**: it drives the document, owns state and input semantics.
- **No RPC**: no request ids, no method names with responses, no correlation. The only request/response pairs are `hello`->`r` and `blobs`->`r`; everything else is one-way. Errors are events, not reply errors.
- Messages Tern -> program are only `r` and `e`, and Tern sends events **only while a surface is open and listening** (or when answering something you sent, unless the program is known gone).

### 4.2 Does Tern spawn the integration? Registration? Configuration?

**No.** A TSP program is simply a process running in a pane (its stdout/stdin are the pty) that probes for Tern with `hello`+DA1 (`protocol/index.md`, `handshake.md`). Consequences:

- No manifest, no config file, no `tern install`, no CLI registration for a program. The "registration" is the `hello` `app` (display name/icon) and the `o` `title`/`role`.
- Tern's session daemon keeps/replays surfaces via synthetic messages (see section 9); it does not launch the program.
- A program is expected to leave cleanly: close surfaces with `x` before it stops reading, then drain input briefly so replies don't reach the next pty reader (usually the shell).

**The other integration path: Tern plugins.** Tern also supports *plugins* (Luau), which are registered by manifest. Example `plugins/examples/jsonx/plugin.toml`:

```toml
schema = 1
id = "jsonx"
name = "JSON Explorer"
version = "1.0.0"
description = "Browse JSON files as a collapsible tree and copy jq paths."
icon = "braces"
host = "host.luau"
window = "window.luau"

[[blocks]]
id = "explorer"
title = "JSON Explorer"
files = ["*.json", "*.geojson"]
palette = false
```

Plugins are installed with `tern plugin install github.com/stencil-hq/tern-sdk/plugins/examples/jsonx` (repo README), return views (they do not speak TSP directly for their blocks/lenses; "Tern speaks TSP for a plugin's blocks and lenses"), and their `hello` sends `features: []` (`elements/input.md`), so **no `edit`/`undo`/`send`, no native selection, controls in a closed block surface don't flip**. `[INFERENCE]` For acp-tsp, the correct model is the *program-in-pane* path (full-featured), not the plugin path; plugins would block the `send`/native-editing features the bridge likely wants.

Tern's own "command lens" system (`block` kind) wraps a lens view in a terminal command block; `surface-play`, `tree`, `css` control-socket commands exist for debugging (`operations.md#debugging`) but are not needed to run a program.

The closest thing to Tern *initiating* work is the `send` feature: "Tern can submit a prompt into your composer atomically (an initial prompt for an agent, Carly asking it): a `send` event..." (`input.md`). That is Tern (or its plugins) pushing a user prompt into the bridge's composer.

---

## 5. Message catalog

### 5.1 Program -> terminal

**`q` query.** SDK `wire::Query` (untagged manual `Serialize`):

```rust
pub enum Query {
    /// `{"q":"hello","v":[1],"app"?,"ver"?,"features"?}`.
    Hello { app: Option<String>, ver: Option<String>, features: Vec<String> },
    /// `{"q":"blobs","ids":[…]}`.
    Blobs { ids: Vec<String> },
}
```

**`o` open** — SDK `wire::Open`:

```rust
pub struct Open {
    pub id: String,              // required; frames/events carry it as `sf`
    pub mode: Option<Mode>,      // inline | screen | flow
    pub title: Option<String>,   // names the pane until program sets OSC 0/2 title
    pub role: Option<String>,    // `data-surface` on the 3 region elements, for stylesheets
    pub listen: Option<bool>,    // false: program never reads input
    pub adopt: Option<bool>,     // true: reopen a closed inline surface
}
```

Doc fields additionally note: unknown mode is an error; an `o` without `id` is an error; `f`/`x` go to the first open surface with that id in the active screen buffer. Semantics in section 6.

**`f` frame** — SDK `wire::Frame`:

```rust
pub struct Frame { pub sf: String, pub s: u64, pub ops: Vec<Op> }
```

`s` increases per surface (from 1), echoed in `ack`/`error`; Tern does not check ordering. Applied atomically: Tern draws the document only between frames.

**`b` blob** — SDK `wire::Blob`:

```rust
pub struct Blob { pub id: String, pub mime: Option<String>, pub data: Vec<u8> }
// Blob::new(data, mime) computes id = lowercase hex SHA-256; MAX_BLOB = 16 MiB
pub fn blob_id(data: &[u8]) -> String
```

**`t` palette** — SDK `wire::Palette`:

```rust
pub struct Palette {
    pub sf: Option<String>,                 // live surface when absent
    pub dark: Option<BTreeMap<String,String>>,  // token -> "#rrggbb"
    pub light: Option<BTreeMap<String,String>>,
    pub name: Option<PaletteNames>,         // { dark?, light? }
}
```

Send right after `o` and before the first `f`. A token ending in `Bg` is a fill; others are text colors. A `t` replaces the previous palette whole. Unknown `sf`/no live surface -> `error`. Stays with the surface after close.

**`s` stylesheet** — SDK `wire::Sheet`:

```rust
pub struct Sheet { pub sf: Option<String>, pub name: String, pub css: Option<String> }
```

`name` 1–64 of `A-Z a-z 0-9 _ -`. Null/absent/empty `css` removes the sheet. Max 256 KiB of CSS per surface (all sheets). Confined to the surface; `url()`, `position:fixed` dropped+reported; `@font-face` ignored; `@import` unsupported; parse errors -> `{"ev":"error","sf","sheet":name,"msg":"<line>:<col>: …; …"}`.

**`x` close** — SDK `wire::Close`:

```rust
pub struct Close { pub id: String, pub keep: bool }
```

`keep:true` (default) leaves `main` in scrollback; `keep:false` removes. `screen` surfaces are always discarded on close.

**`wire::Message`** envelope + `verb()` (`q,o,f,b,t,s,x`), `body()` (JSON, or base64 for blob), `params()` (only `mime` for a blob).

### 5.2 Terminal -> program

**`r` reply** — SDK `wire::Reply`: `Hello(Hello)`, `Blobs(BlobsReply)`, `Unknown(Map)`. `Hello` (camelCase, `raw` kept):

```rust
pub struct Hello {
    pub v: u32, pub term: String, pub ver: String,
    pub kinds: Vec<String>, pub features: Vec<String>,
    pub apc: usize, pub credits: u32, pub cols: u32,
    pub cell: Option<Cell>, pub dark: bool,
    pub reduce_motion: bool, pub hour12: bool,
    pub raw: Map<String, Value>,
}
```

`BlobsReply { have: Vec<String>, raw }`.

**`e` event.** The full set (doc `input.md` + SDK `wire.rs` structs). Every event but `theme`, `motion`, and a `gone` answering an `adopt` carries `sf`; `error` has `sf` when the rejected message named a surface.

| `ev` | Fields (SDK struct) | When |
|---|---|---|
| `ack` | `sf`, `s` — `Ack` | highest frame applied *and drawn*; at most one per displayed frame |
| `resize` | `sf?`, `cols`, `cell?`, `visible?` — `Resize` | pane's live surface first drawn, or width/cell changed |
| `theme` | `dark: bool` — `Theme` | appearance switched |
| `motion` | `reduce: bool` — `Motion` | Reduce Motion toggled |
| `visible` | `sf?`, `visible: bool` — `Visible` | pane hidden/shown |
| `toggle` | `sf?`, `id`, `collapsed`, `key?` — `Toggle` | collapsible node folded; `key` is the bare node `key` prop (or a tree item) |
| `select` | `sf?`, `id`, `item`, `values?` — `Select` | item picked (list item: `id`=list, `item`=item) |
| `activate` | `sf?`, `id`, `item`, `values?` — `Select` (variant `Activate`) | item opened (double click) |
| `action` | `sf?`, `id`, `act`, `value?`, `mods: Vec<String>`, `values?` — `Action` | custom pointer action; `mods` order `shift,ctrl,alt,meta`; action name `"sort=name"` splits into `act`/`value` |
| `change` | `sf?`, `id`, `value`, `checked?`, `name?`, `item?`, `values?` — `Change` | `el` checkbox/radio flip, or `prefs` row change |
| `focus` | `sf?`, `id` — `Focus` | a click asked for keys in a field |
| `edit` | `sf?`, `id`, `from`, `to`, `text`, `cursor`, `len` — `Edit` | native editing: replace UTF-16 `[from,to)` with `text`, caret at `cursor`, `len`=length Tern saw (ignore if your text changed). Only with `edit` feature |
| `undo` | `sf?`, `id` — `Undo` | undo last change through your history. Only with `undo` feature |
| `send` | `sf?`, `id`, `text` — `SendText` | submit `text` through composer `id`'s normal path; only to a live, editable `sendable:true` field of a `send` program |
| `error` | `sf?`, `s?`, `op?`, `msg`, `sheet?`, `id?` — `ErrorEvent` | rejected op (with `s`,`op` index), rejected message/dropped chunk, stylesheet parse errors (`sheet`), `el` unknown tag (`id`) |
| `gone` | `sf?`, `ids` — `Gone` | Tern dropped nodes under retention, dropped a surface whose anchor left, or an `adopt` found nothing |

Unknown events, or known ones whose fields don't fit, decode as `Event::Unknown(Map)` (SDK `Event::decode`), so forward compatibility is automatic (`Event::name()`, `raw()`, `sf()`, `id()` helpers).

**Events only reach a listening program.** Events Tern originates go out only while some surface is open (and, if they name a surface, only while that surface is open). A `listen:false` surface never hears anything; after its `o` no answers at all until your next `q` or listening `o`. Answers to what you sent go out unless the program is known gone (after a shell prompt, process exit, or RIS). A program that closed its own surfaces with `x` still hears about its mistakes.

---

## 6. Surfaces

### 6.1 Modes (`surfaces.md`)

| Mode | For | Sits | After the program |
|---|---|---|---|
| `inline` (default) | a session program in the pane | at the cursor row, owning the pane while live (clean pane) | discarded at next shell prompt |
| `screen` | a full-screen view | over the whole pane, like alt screen | discarded on close |
| `flow` (feature `flow`) | a non-full-screen CLI that prints/asks/progresses then exits | at cursor row among output | kept in scrollback as output |

- **Inline**: anchored at cursor row of the main screen; while live it owns the pane (shell rows/scrollback hidden, nothing scrolls above its first row). Opening one on the alternate screen is an error.
- **Screen**: fills the pane incl. overlays; discarded on close regardless of `keep`.
- **Flow**: like inline anchor, but never owns the pane; closing (`x keep:true`, or program exit) keeps it as command output; kept until its anchor row leaves scrollback. Live surface with frames/events/`dock`/`layer`.
- **`listen:false`** (any mode): program never reads input; Tern sends nothing about the surface (no acks -> send frames without waiting for credits, no events, no errors, no answers until your next `q` or listening `o`); controls don't flip; no raw mode needed.
- **Screen over inline**: a screen surface covers the pane; pointer/caret/view events go to the screen surface; the inline surface's document/dock/layer/focus/seq stay untouched; frames addressed to it still apply; on screen close the inline shows exactly as before.
- **Replacement**: new `o` replaces the buffer's live surface of the same mode (inline/flow closed as `keep:true`, screen discarded). A new inline `o` also discards a covering screen surface. A screen `o` never touches inline; neither touches flow.
- **Adopt** (feature `adopt`): `o {id, adopt:true}` reopens the newest closed inline surface with that id still in the pane's scrollback: same document, fully editable; no `dock`/`layer` (dropped on close); `mode` ignored; `title`/`role` replace when given; `listen` applies. Like a new inline `o`, it closes the buffer's live inline surface and discards a screen surface; adopting the already-live id does nothing. If the id isn't in the pane, Tern sends `{"ev":"gone","ids":["s1"]}` (unless `listen:false`) and you open a new surface.
- **Suspend/resume**: frame ops `["suspend"]`/`["resume"]` hand the pane back to the grid (interactive shell command, `$EDITOR`). A suspended surface keeps its place but takes no pointer focus; grid output appears below.
- **Crash safety**: process exit closes every live surface (inline/flow as `keep:true`, screen discarded). RIS (`ESC c`) clears the screen and its surfaces. The program is then "known gone" and Tern sends nothing until a `q` or `o`.
- **Back at the prompt** (OSC 133;A): Tern discards every inline and screen surface of the pane, including `keep:true` ones. Flow surfaces and Tern's own command-lens blocks stay.
- **Leaving**: close surfaces with `x` *before* you stop reading input, then drain input briefly, so replies/events in flight don't reach the shell (which would type them: zsh reads `ESC _` as insert-last-word).
- **Title**: when a command opens its first listening surface, Tern drops the shell title; `o.title` names the pane until the program sets its own title.
- **Stray output**: non-TSP bytes while a surface is live (logging, stderr) go to the grid as usual; while inline owns the pane they stay hidden until it closes/suspends.
- **Alternate screen**: inline/flow belong to the main screen; a screen surface opened on alt screen goes when the program leaves/clears alt screen, and a listening program hears `gone`.

### 6.2 Regions and anchoring

A surface's root has id == surface id; up to three children with fixed ids: **`main`** (flowing document, scrolls with pane), **`dock`** (sticky at pane bottom while live: editor, status line; removed on close; feature `dock`), **`layer`** (overlays; removed on close). Region elements expose `.sf-region`, `.sf-main/.sf-dock/.sf-layer`, `[data-scope='sf1']`, `[data-surface='<role>']`, `[data-app='<hello.app>']`, and a region node's `role` prop as `[data-role]`. Under a modal/full overlay or picker: `.sf-main.sf-covered`, `.sf-dock.sf-covered`.

A screen surface's `main` takes the height and scrolls itself; `dock` sits under main.

**Aside** (feature `aside`): a `prefs` node in an inline surface's `layer` becomes a sheet docked at the pane's right edge (reserves width, grid/main/dock end beside it, narrower `cols` in a `resize`).

**Anchoring**: an inline/flow surface occupies exactly one grid row (its **anchor**). On `o`, Tern ends the cursor's row and moves the cursor to the row below. The anchor never joins a wrapped line on resize, isn't trimmed as blank, scrolls into history. The surface lives exactly as long as its anchor row; when the row falls off scrollback, is erased (`ESC [2J`, `clear`) or scrolled away in a scroll region, or scrollback is cleared, the surface goes and a listening program hears `gone`.

**Scroll anchoring**: when content above the view changes height, the view stays on the content under the viewport; inside a surface the scroll position is a child of `main`.

---

## 7. Documents, frames, ops

### 7.1 Node model

```json
{"id": string, "k": kind, "p": props, "c": [children]}
```

- Ids are the program's: non-empty strings, unique within the surface, short; keep a component's id stable so Tern keeps its elements/highlighting/folding. `k` must be a kind from `hello.kinds`. `p`/`c` optional. `null` prop is dropped. Text lives in props (`text`, `spans`); text kinds keep primary text in `text`, addressed by `text`/`splice` ops. Nothing inherits font metrics from the program.
- Root is the surface itself (a `col` whose id is the surface id, unmovable/undeletable). Children are the regions, added with ids `main`/`dock`/`layer`. A surface starts with only the root, so the first frame adds the regions it uses.
- SDK derived ids (`reconcile.rs`): region root = `main`/`dock`/`layer`; child = `<parent>.<key>` where `key` is the node's `key` prop (string, or integer in decimal), else its index. Duplicate sibling id -> whole view rejected (`Error::DuplicateId { path, key }`). Empty `c` object/array means no children.

### 7.2 Ops (SDK `wire::Op`, serialized as JSON tuples)

| Op | Shape | Effect |
|---|---|---|
| `add` | `["add", id, parent, before|null, node]` | Insert whole subtree under `parent` before sibling `before` (null=last). `id` must equal `node.id`; `before` required. If any node of the subtree fails (unknown kind, dup id, limit), none added. |
| `set` | `["set", id, props]` | Merge props shallowly (each key replaces whole; `null` deletes). |
| `text` | `["text", id, "append"|"replace", string]` | Update primary text of a text kind (text, md, code, ansi, math, editor, input, shimmer, el). `append` to a node without text sets it; `append` is the streaming path (`md` re-renders from its last open block). |
| `splice` | `["splice", id, at, del, string]` | Replace `del` UTF-16 code units at `at` (editor edits/large pastes). `at+del` within text; neither end may split a surrogate pair. |
| `move` | `["move", id, parent, before|null]` | Reparent/reorder; `before` a child of parent or null; can't move into own subtree. |
| `del` | `["del", id]` | Remove subtree; focus inside goes with it. |
| `settle` | `["settle", id]` | Hint subtree won't change soon; Tern may flatten/free view state. Under retention, the oldest settled children of `main` are dropped first with `gone`. Feature `settle`. |
| `focus` | `["focus", id|null]` | The `editor`/`input` owning caret and IME, or none. |
| `reveal` | `["reveal", id, "start"|"end"|"nearest"]` | Scroll a node into view (a `list` scrolls to it; `main` scrolls the pane; dock/layer only their containers). `start` honors `scroll-padding`; other positions read as `nearest`. Only the latest reveal applies. |
| `scroll` | `["scroll", id, "line-up"|"line-down"|"page-up"|"page-down"|"start"|"end"]` | Keyboard scrolling you forward; moves nearest scroll container at/above node. Feature `scroll`. |
| `suspend`/`resume` | `["suspend"]`, `["resume"]` | Hand the pane back to grid / take it again. |

An op naming an unknown id, adding an unknown kind, breaking a limit or malformed is rejected **alone** with an event naming its 0-based index (`{"ev":"error","sf":"s1","s":2,"op":1,"msg":"unknown id t7"}`); the rest of the frame applies. A frame Tern can't take at all (no `sf`, unknown/closed surface, `ops` not an array, body not JSON) is dropped whole with an `error` without `op`.

### 7.3 Time and animation

- No per-frame updates: motion kinds (`spinner`, `shimmer`, `elapsed`, `rate`) and span effects (`fx`) are clocked by Tern and pause when hidden.
- **Ages, not times**: `elapsed.age` (and a `tool`'s `age`, an `agent`'s `stats.age`/`tool.age`/`retry.age`) is ms already elapsed when the frame was written, negative for countdowns; Tern stores `start = receive − age`. Patch of other props leaves the count running; replay rewrites ages.
- Reduce Motion turns motion into still equivalents; `hello.reduceMotion` and `motion` events tell you.

---

## 8. Input and events (interaction semantics)

### 8.1 Keys

Keys reach the program as pty input (kitty keyboard protocol, bracketed paste). A focused `editor`/`input` shows Tern's caret, but the text model is yours: answer keys with `text`/`splice` ops and a new `cursor`. IME composition happens in Tern's hidden text area, is drawn as preedit at the field's caret, and commits to the pty as typed text.

**Native editing** (program `edit` feature): Tern keeps its own selection in your `editor`/`input` unless the field has a `mode` (e.g. Vim) or is `readonly`, and unless the user disables it. Selecting keys, drag/double/triple click, ⌘A; when a selection exists, typing/paste/IME/backspace/delete/⌘X/⌃X replace it, ⌘C copies locally, plain arrow collapses; a click in the focused field sends an `edit` with `from==to` only if the caret moved; a click in an unfocused field sends `focus`. Plain ↑/↓ move the caret to the drawn line above/below (soft-wrap aware), ⌘←/→ (Home/End) to drawn line start/end — each one `edit` event, never keys. **Undo** (program `undo` feature): ⌘Z/⌃Z sends one `undo` event with or without selection and in an empty field; the history is yours, each `edit` one step.

**Sending** (program `send` feature): Tern can submit a prompt into your composer atomically via a `send` event, not paste+Enter. Accepted only while the field reports `sendable:true` and is not `readonly`/`disabled`/`hidden`. Writable text and focus are separate; send a frame setting `sendable:true` when ready even if nothing else changed; Tern holds its prompts until then.

### 8.2 Pointer actions

Common `actions` prop: `{"click": act, "dblclick": act, "menu": [act…]}` (every key optional). Actions:

- `toggle`: Tern folds/unfolds at once -> `{"ev":"toggle","id","collapsed","key?"}` (key = bare node `key`; local fold remembered by key among siblings, else id).
- `copy`: Tern copies the node's text (Markdown source for `md`, raw text for `code`/`ansi`/`text`, a `block`'s raw output, the `href` when it has one), flashes, shows Copied. No event.
- `open`: opens the node's `href`/`path`. No event.
- `zoom`: shows the `image` in its viewer. No event.
- `select` -> `{"ev":"select","id","item","values?"}`; `activate` -> `{"ev":"activate","id","item","values?"}`.
- any other name -> `{"ev":"action","id","act","value?","mods?","values?"}`; a name with `=` splits into `act`/`value`; `mods` order `shift,ctrl,alt,meta`, absent when none. `values` present when the node sits in an `el` `form`.

Kinds with their own events without `actions`: `list` items (`select`/`activate`, `id`=list), `tabs` (`select` with `item`=tab id), `tree` (`toggle` with `key`=item), `picker` (`select`/`activate`/`action`), `prefs` (`select`/`activate`/`action`/`focus`/`change`), `checklist` hud (`activate` with `item="hud"`), `el` checkbox/radio (`change`), `editor`/`input` (`focus`/`edit`/`undo`).

Hover, scrolling, text selection, tooltips and the context menu are Tern's own and never reported.

### 8.3 Form values (`input.md#form-values`)

An `action`/`select`/`activate`/`change` from a node inside an `el` `form` carries `values`: each enabled, named control of that form at that moment. Radio group -> checked radio's `value` (null when none); lone checkbox -> `true`/`false`; same-name checkboxes -> array of checked values in document order. The click that submits carries what was picked, so you never have to ask:

```json
{"ev":"action","sf":"pick","id":"go","act":"submit","values":{"size":"l","notify":true,"tags":["a","c"]}}
```

---

## 9. Element vocabulary (the UI surface primitives)

44 kinds (SDK `wire::kind::ALL`). Model: `Node { k, p?, c? }`; each node becomes one element (`div`, or `span` for inline kinds; `el` uses its tag) with classes `sf sf-<kind>`, `data-id`, plus common props as `data-role`/`data-tone`/`data-mark`/`data-href`/`title`/`aria-label`/`sf-hidden`/`sf-act`/`sf-clip`. Region roots draw no element; children go into `.sf-region.sf-main|dock|layer`.

**Common props** (all kinds): `key`, `role`, `tone` (neutral|accent|info|success|warning|error|pending|muted|user -> `data-tone`, sets `--tc`), `hidden`, `mark` (pick|drop), `actions` ({click,dblclick,menu}), `title`, `aria`, `href`, `grow`, `shrink`, `basis`, `min`, `max`.

**Text kinds** (primary `text`, addressable by `text`/`splice` ops): `text`, `md`, `code`, `ansi`, `math`, `editor`, `input`, `shimmer`, `el`. (`diff`, `rows` are not.)

| Kind | Draws | Key props |
|---|---|---|
| `col` | vertical flex stack | `gap` none/xs/sm/md/lg (0/2/6/10/16px), `align`, `justify` (between/end; disables virtualization), `wrap`; large cols (>64 children or ~3 viewports) virtualize unless `justify`/`wrap` |
| `row` | horizontal flex row, children centered, each `min-width:0` | `gap`, `align` (baseline useful), `justify`, `wrap`; never virtualizes |
| `card` | rounded framed box: icon(role)/head/status chip/chevron + body | `head` (spans or child id), `status` pending/running/done/error/cancelled, `collapsible`, `collapsed`, `preview{lines}`; body indent; SDK also `selected`,`inset`,`variant:bare` |
| `section` | unframed disclosure group | `head`, `collapsible`, `collapsed` |
| `rule` | hairline divider | `label` |
| `spacer` | vertical space | `size` (gap scale) |
| `text` | styled spans | `spans`/`text`, `wrap` word/char/none, `truncate` end/start/middle, `lines`, `measure:"prose"` |
| `md` | Markdown (omp dialect: callouts, tables, tasks, math, mermaid) | `text`, `stream`, `marks` |
| `code` | highlighted code | `text`, `lang`, `path`, `numbers`, `start`, `marks`, `wrap` |
| `diff` | unified/split diff | `text` or `hunks`, `path`, `lang`, `mode` auto/unified/split |
| `ansi` | mini terminal (SGR, OSC 8) | `text`, `cols`, `follow`, `preview{lines}` |
| `rows` | pre-rendered ANSI rows (fallback) | `lines`, `cols` |
| `math` | TeX | `text`, `display` |
| `kv` | aligned key/value list | `items:[{k,v}]`, `layout` grid/inline |
| `table` | column grid, priority-hidden cols, meter cells | `cols:[{id,head,align,truncate,priority,grow}]`, `rows:[{id,cells:{col: Spans|{meter}}}]` |
| `tree` | disclosure tree | `nodes:[{id,label,icon,open,children}]` |
| `badge` | tone pill | `text` |
| `kbd` | keycaps | `keys:[…]` |
| `icon` | named icon | `name` (Tern icon set) |
| `image` | image from a blob | `blob`, `builtin`, `alt`, `w`, `h`, `path`; children drawn over |
| `list` | listbox of `item`s | `selected`, `filter` (marks only), `empty`, `virtual`; `max` via `{lines}` or fraction; self-scrolls when capped |
| `item` | one row | `label`, `detail`, `value`, `icon`, `hint`, `disabled`, `tone` |
| `tabs` | tab strip | `items:[{id,label}]`, `active` |
| `picker` | modal searchable picker sheet | huge: `size`, `layout`, `preview`, `query`, `cursor`, `items`,`itemsAdd`,`itemsDel`,`order`,`selected`,`current`,`hits`,`columns`,`confirm`,`scopes`,`scope`,`tabs`,`tab`,`strip`,`actions` |
| `spinner` | activity indicator | `style` braille/dots/starburst/orbit, `label`, tone |
| `shimmer` | sweeping label | `spans`/`text`, `mode` classic/kitt, `palette{low,mid,high}` |
| `elapsed` | live timer | `age` ms (negative=countdown), `stopped`, `format` short/clock |
| `rate` | easing number | `value`, `unit` |
| `progress` | progress bar | `value` 0–1 (indeterminate without), `label` |
| `meter` | value as bar/ring/block grid | `value`, `parts`, `thresholds{warn,bad}`, `style` bar/ring/blocks, `size`, `steps`, `marks`, `label`, `total` |
| `chart` | bars/spark/heatmap | `kind`, `size`, `token`, `summary`, `series`, `cells`,`tips`,`rows`,`cols` |
| `effort` | thinking-effort glyph | `level` off/minimal/low/medium/high/xhigh/max |
| `editor` | multi-line field (program owns text) | `text`,`cursor`,`anchor`,`decor`,`ghost`,`placeholder`,`prompt`,`mode`,`lang`,`highlight`,`readonly`,`sendable`,`maxLines`,`nowrap`,`topline`,`followCursor`,`gutter`,`virt` |
| `input` | single-line field | same minus maxLines/topline/etc. |
| `status` | 24px strip | `transparent`; children `seg`s, right group via `side="right"` |
| `seg` | status segment | `text`/`spans`, `icon`, `tone`, `side`, `priority`, `min:{w}` |
| `toast` | transient notice | `text`, `sub`, `tone` (error/info/success), `ttl` ms |
| `overlay` | floating glass panel in `layer` | `anchor` center/top/bottom/{node,side}/{caret}, `size` sm/md/lg/full, `modal`, `head` |
| `tool` | one coding-agent tool call | see below |
| `agent` | subagent row with live stats | see below |
| `checklist` | phased todo list / hud pill / reminder | see below |
| `block` | Tern's own command-lens block | Tern-built; `native`+`raw`, Native/Raw toggle, Copy |
| `prefs` | settings page/sheet | `pages`,`page`,`lead`,`sections`,`query`,`cursor`,`focus`,`editing`; 7 control kinds switch/choice/number/text/keys/multi/action |
| `el` | one HTML element from a fixed tag list (+checkbox/radio) | `tag`, `class`, `attrs`, `text`, `type`, `name`, `value`, `checked`, `disabled` |

### 9.1 `tool` (key for ACP tool calls) — `elements/work.md#tool`

Props: `name` (icon), `title` (verb), `target`, `targetKind` command|path|pattern|query|text, `href`, `meta:[text]`, `badges:[{text,tone,title}]`, `note`, `exit` (non-zero -> `exit N` chip), `status` pending|running|done|error|cancelled, `age`/`took` (ms), `intent`, `frame` card|inline, `collapsible`/`collapsed`, `preview{lines|tail}`, `tools:[{id,label,keys}]` (hover buttons), `key`. Children are the body (any kinds; nested card/tool lose frames; code/diff lose background). Events: `toggle`, `action` (custom tool button ids), plus common `actions`. The head shows status bar, icon, verb, target, meta, badges, note, exit chip, timer, status glyph, chevron, hover actions. "Fold tool calls" setting groups consecutive work blocks under one head.

### 9.2 `agent` (subagents, usage/cost) — `work.md#agent`

Props: `name`, `agent` (badge), `badges`, `task`, `status` pending|running|done|failed|aborted|idle|parked, `model`, `thinking` (palette token for dot), `stats{tools,requests,done,context,contextLabel,tokens,cost,age,took}`, `tool{name,intent,age}` (line 2), `retry{attempt,max,delay,age,error}`, `depth` 0–6, `collapsible`/`collapsed`. Stats render: `stats.tools` -> wrench+count; `requests` -> "N req"; `done` -> ring+N%; `context` -> meter (warn 75%, bad 90%); `tokens` -> compact (12K/1.2M); `cost` -> `$0.42`/`<$0.01`; `age`/`took` -> live/frozen timer. Events: `toggle`.

### 9.3 `checklist` (plans/todos) — `work.md#checklist`

`phases:[{id,title,items:[{id,text,status,note}],collapsed}]`, `mode` full|hud|reminder, `note`. Item status pending|active|done|dropped|blocked (unknown -> pending); progress counts done+dropped. Events: `activate {id,item="hud"}` for the hud pill; phase folds are local.

### 9.4 `prefs` (modes/models/config) — `work.md#prefs`

Full settings page or sheet; controls: switch, choice (segmented/menu), number (stepper), text, keys, multi (chips/ordered), action. Events: `action {act}` (page/close/control), `change {item,value}`, `activate {item}`, `select {item}` (+ `focus`). Keys never reach the page; drive via `focus`/`editing` props from your key handler.

### 9.5 `el` (forms/permissions dialogs) — `elements/el.md`

Tags: div, span, p, section, header, footer, nav, aside, main, article, figure, blockquote, ul, ol, li, dl, dt, dd, h1–h4, pre, code, kbd, strong, b, em, i, del, mark, hr, table, thead, tbody, tr, th, td, label, button, form, input(checkbox|radio). No `a`/`img`/`textarea`/`select`. `attrs` allowlist: `data-*`, `aria-*`, `role`, `aria-labelledby`, `colspan`, `rowspan`. `form` draws a div but groups controls; controls are pointer-only (no tabindex), click flips locally then sends `change`; radio groups by `name` within nearest form. `values` on events from inside a form (section 8.3). Raw HTML in Markdown stays literal; no scripts.

### 9.6 Spans and style tokens — `elements/text.md#spans`

Span: `{t, s?, href?, fx?}`. `s` = space-separated tokens: `muted dim strong em accent success warning error info code mono path key link num ins del mark typo icon hide` + omp/palette tokens (mapped to semantic tokens, inline `color: var(--sf-p-<token>, fallback)`). `fx` shimmer|pulse|none. No raw colors. Nerd Font PUA glyphs auto-split into `icon` spans.

### 9.7 Theme/palette, stylesheets, icons

- `t` palette sends `{dark,light:{token:#rrggbb},name}`; semantic tokens/tones resolve through it (`accent/success/warning/error/muted/dim` same name; `ins`->toolDiffAdded/success; `del`->toolDiffRemoved/error; `link`->mdLink/accent; `path`->statusLinePath/mdLinkUrl; `code`->mdCode; `num`->syntaxNumber; `info`->syntaxType; `mark` bg->selectedBg; fills become 60% tints). Tern corrects contrast (4.5:1 body, 3:1 quiet).
- `s` stylesheets: CSS confined to the surface (region elements are outside; `.sf-main` inside a sheet matches nothing there — the docs contradict themselves slightly: `theme.md` says selectors are relative to the surface regions and `.sf-main` is outside; `surfaces.md` documents the region elements). `[INFERENCE]` Region element selectors are targetable only by Tern's own/internal sheets; program sheets target inside. No `url()`, `position:fixed`, `@font-face`; `@keyframes` scoped.
- Icons: named icons via `icon` kind / `item.icon` / `seg.icon` / role; glyphs in text via Nerd codepoints + `icon` token (or auto-detected in prose/editors).

---

## 10. Flow control, retention, persistence, debugging, limits

### 10.1 Flow control (`operations.md#flow-control`)

- Keep at most `credits` frames (2) unacknowledged. `ack` carries the highest drawn `s` and covers every frame up to it; Tern acks once per drawn frame, so a frame applied+drawn together with the next is never acked alone. While waiting, reconcile into a pending op list (collapse sets, merge appends) and send one frame on ack.
- The window that draws the surface acks; while detached, no acks arrive (hold pending ops). `listen:false` surfaces get no acks (send without credits).
- Streaming: `text append` on `md`/`ansi`/text nodes; only new bytes go on the wire.
- Settling: after `settle`, stop diffing that subtree; later changes via targeted ops; a later op clears the hint on the node and ancestors, so re-`settle`.
- Ages instead of timestamps.

### 10.2 Retention

Per-pane surface budget: 64 MiB of document state by default (estimate of ids/props + per-node overhead, summed over surfaces/blocks). Each frame/palette/stylesheet/close over budget evicts: (1) closed surfaces/finished blocks whole, oldest first; (2) oldest settled top-level children of `main` of open surfaces, reported with `gone`; a later op on an evicted id is an unknown node. When nothing settled is left, the pane stays over budget.

### 10.3 Persistence

- The pane's screen owns the documents; in a Tern session the session daemon's screen holds them and every attached window receives the same bytes (survives window restart).
- A window attaching gets a snapshot that replays, through the normal parser: a `hello` with your `app`/`features`, then each surface (closed first) as a synthetic `o` (mode/listen/title/role), its palette `t` and stylesheets `s`, one frame with your last `s` adding the whole document followed by `settle`s, focus, `suspend` when suspended, then `x keep:true` for closed surfaces. Ages rewritten to snapshot time. Tern's answers to the replay don't reach your program.
- On detach/reattach, a live surface carries on (you keep writing into the pty within credits; the new window draws the current document).
- A pane restored after daemon stop starts a new shell; inline/flow surfaces and command blocks come back closed in the scrollback (read-only); `screen` surfaces don't come back.
- Blobs kept on disk by SHA-256 in Tern's cache (`blobs/`), so snapshots show images without resending.
- View-only state (folds, control flips, scroll positions) is the window's and not snapshotted; mirror what matters into props.

### 10.4 Debugging

- Recording: log every message with timestamp as JSONL `{"t":ms,"dir":"out"|"in","verb":"f","params":{…},"body":{…}}` (omp uses `PI_TUI_TSP_RECORD=<file>`; SDK uses `TERN_TSP_RECORD`).
- Replay: Tern's `surface-play "<file.jsonl>" [from <frame>] [until <frame>] [paced [<times>]]` feeds a recording's `out` messages into the focused pane. Frame selector `s` or `<sf>#<s>`. Hand-written `{"dir":"out","verb":"osc","body":"2;Title"}` and `pty` lines supported. First play restarts the pane as a stand-in program that reads+ignores answers.
- Inspect: control socket `tree "[data-surface='omp.session']"` lists a surface's elements; `css` lists installed sheets (yours as `tsp:<scope>:<name>`).

### 10.5 Security and limits

No scripts, no raw HTML; `el` fixed tag list + attribute allowlist; CSS confined and cannot load/escape; links behave like OSC 8 (opened on explicit gesture); `file://` limited to paths; events carry only ids the program created plus user-chosen values; typed text reaches you as pty input (exceptions: native `edit` events with `edit` feature, and `send` with the `send` feature).

| Limit | Value |
|---|---|
| One blob | 16 MiB decoded |
| Blobs per screen | 256 MiB, unreferenced LRU-evicted |
| Blobs on disk | 1 GiB, LRU at daemon start |
| Nodes per surface | 200000 incl. root |
| Depth | 64 |
| Stylesheets per surface | 256 KiB CSS total |
| One APC string | 262144 bytes; Tern abandons longer silently. Chunk at `apc` (65536) |
| One chunked message | 24 MiB joined; 1 s idle or interruption drops with `error` |
| Surface state per pane | 64 MiB default |

---

## 11. Rust SDK: `tern-sdk`

### 11.1 Package facts

- Crate: **`tern-sdk`**, library name `tern_sdk` (`rust/Cargo.toml`).
- Version **0.1.0**, published on **crates.io 2026-10-05** (https://crates.io/crates/tern-sdk), `publish = true`, MIT, repository https://github.com/stencil-hq/tern-sdk, authors Stencil Labs, Inc.
- `edition = "2024"`, no `rust-version` field -> **MSRV at least Rust 1.85** `[INFERENCE]` (edition 2024 requires 1.85).
- **Synchronous, no async runtime, no tokio.** README: "Synchronous, no async runtime: `serde`, `serde_json`, `sha2`, `base64`, `thiserror`, `tracing`, and `libc` / `windows-sys` for the tty."
- Deps: base64 0.23.1; serde 1.0.229 (`alloc`,`derive`,`rc`); serde_json 1.0.151 (`float_roundtrip`,`preserve_order`,`raw_value`); sha2 0.11.0; thiserror 2.0.21; tracing 0.1.44 (`std`,`attributes`); unix: libc 0.2.189; windows: windows-sys 0.61.2 (huge feature list: console, IME, DWM/GDI, pipes, job objects...).
- Platform: wire/input/node/reconcile layers build on desktop (macOS, Linux, Windows), iOS and wasm32-unknown-unknown; process-tty APIs (`term::Tty`, tty detection, `Session::connect`, `print`, `ask`) only Unix+Windows (target-gated, not stubbed on wasm). `term::Terminal` and `Session::with_terminal` (custom transport) remain available everywhere.

### 11.2 Public surface (crate root, `lib.rs`)

```rust
mod helpers; pub mod input; pub mod keys; mod plain; pub mod reconcile;
mod record; mod session; pub mod term; pub mod ui; pub mod wire;

pub use helpers::{Answer, AskOptions, PrintOptions, Submission, ask, ask_with, print};
pub use keys::Key;
pub use plain::plain;
pub use session::{Capabilities, Input, Options, Session, Surface, SurfaceOptions};
pub use ui::{Node, View};
pub use wire::{Event, Mode, Op};

pub enum Error { Io, Json, DuplicateId{path,key}, InvalidView(String),
                 BlobTooLarge(usize), SurfaceClosed(String), Timeout(&'static str), InputClosed }
```

Six layers (lib.rs doc, README):

1. `tern_sdk::wire` — constants/catalog (`VERSION=1`, `APC_LIMIT=65536`, `CREDITS=2`, `MAX_BLOB=16<<20`, `kind::*` incl. `ALL`/`TEXT_KINDS`/`has_text`, `feature::*`), messages (`Query`,`Open`,`Frame`,`Op`,`Blob`,`Palette`,`Sheet`,`Close`), encoder (`encode`, `chunks`, `Encoder`), decoded `Reply` and `Event` (typed variants + `Unknown`, `raw()` is the JSON as received).
2. `tern_sdk::input`, `tern_sdk::keys` — `input::Parser` splits replies/events/DA1 out of pty input and passes keys through (bracketed-paste-aware, `MAX_MESSAGE = 32 MiB`); `keys::Decoder` -> `Key` (`key.is("ctrl+c")`, `Key::typed()`).
3. `tern_sdk::ui`, `tern_sdk::ui::html` — a builder per kind (`ui::card()`, `ui::table()`, `ui::editor()`, …) with typed props, common props, `Text` as string or `spans![…]`, typed enums with `Other` escape hatch, `el` builders per tag (`html::form()`, `html::radio(name,value)`, …), and `.prop(name,value)` / `ui::node(kind)` untyped.
4. `tern_sdk::reconcile` — `Doc` (a view with derived ids) and `doc.ops(&next, surface)`, Tern's plugin algorithm; `REGIONS`, `Tree` (id/kind/props/children/wire), `Routes`/`Handler` routing.
5. `tern_sdk::Session` — handshake, raw mode, bracketed paste + kitty keys, surfaces, credits, blobs, routing, recording, close.
6. Helpers — `print`, `ask`, `plain`.

### 11.3 Key types

**`Session<M = ()>`** (`session.rs`):

```rust
pub fn connect(options: Options) -> Result<Option<Self>, Error>  // Unix/Windows; None when no TSP
pub fn with_terminal(term: impl Terminal + 'static, options: Options) -> Result<Option<Self>, Error>
pub fn caps(&self) -> &Capabilities
pub fn open(&mut self, options: SurfaceOptions) -> Result<Surface, Error>
pub fn surface_id(&self, sf: Surface) -> &str
pub fn is_open(&self, sf: Surface) -> bool
pub fn render(&mut self, sf: Surface, view: impl Into<View<M>>) -> Result<(), Error>
pub fn send(&mut self, sf: Surface, ops: Vec<Op>) -> Result<(), Error>   // raw ops, queued while out of credits
pub fn focus / reveal / scroll / settle / suspend / resume (sf, …) -> Result<(), Error>
pub fn stylesheet(&mut self, sf, name, css: Option<&str>) -> Result<(), Error>
pub fn palette(&mut self, sf, Palette) -> Result<(), Error>
pub fn close_surface(&mut self, sf, keep: bool) -> Result<(), Error>
pub fn blob(&mut self, data: Vec<u8>, mime: Option<&str>) -> Result<String, Error>  // once per session
pub fn blobs(&mut self, ids: Vec<String>) -> Result<Vec<String>, Error>            // 2 s timeout
pub fn next(&mut self, timeout: Option<Duration>) -> Result<Option<Input<M>>, Error>
pub fn pump(&mut self, timeout: Duration) -> Result<(), Error>                     // process acks without returning
pub fn close(mut self) -> Result<(), Error>
// Drop impl calls shutdown(): closes open surfaces, resets input modes, 50 ms drain, restores tty.
// On Unix SIGTERM/SIGHUP/SIGINT/SIGQUIT restore the tty (async-signal-safe) then re-raise.
```

**`Input<M>`**: `Key(Key)`, `Event(Event)`, `Msg(M, Event)`. Acks are consumed inside.

**`Options`**: `app`, `version`, `features: Vec<String>`, `timeout` (hello, default 1 s), `bracketed_paste` (default true), `kitty_keyboard` (default true), `record: Option<PathBuf>` (defaults to `TERN_TSP_RECORD`). Builder methods `.app()/.version()/.feature()/.timeout()/.bracketed_paste()/.kitty_keyboard()/.record()`.

**`Capabilities`**: `term, version, kinds, features, apc, credits, cols, cell, dark, reduce_motion, hour12` + `has_kind()`/`has_feature()`. Handshake copies `hello`, sets the encoder limit to `apc`, enables bracketed paste/kitty flags.

**`SurfaceOptions`**: `id` (default `s1,s2,…`), `mode`, `title`, `role`, `listen` (default true), `adopt`, `keep` (default true); constructors `inline()/screen()/flow()`.

**`Surface(usize)`** handle (only valid with its session; another session's handle panics).

**Handlers** (`ui::node::Trigger`, README table): `.on_click(msg)`, `.on_dblclick(msg)`, `.on_menu(act,msg)`, `.on_action(name,msg)`, `.on_action_with(name,|a|…)`, `.on_toggle(|collapsed|…)`, `.on_select(|item|…)`, `.on_activate(|item|…)`, `.on_change(|c|…)`, `.on_edit(|e|…)`, `.on_send(|text|…)`, `.on_focus(msg)`, `.on_undo(msg)`. Handlers map events to messages `M` (no side-effecting closures); routing is by `sf`+`id` to the node in the last rendered view (for list select/activate: item's own handler first, then the list's). `Session::next` -> `Input::Msg(msg,event)` when handled, `Input::Event(event)` otherwise, `Input::Key(key)` for keys.

**Flow control in the SDK**: at most `credits` unacked frames per listening surface; `render` while blocked only stores the view (and queues raw ops); the difference goes out when an ack arrives (during `next`/`pump`). `close_surface` first sends what still waits as one last frame, then `x`. Frame writes commit view/seq/queued ops only on success; close still attempts `x`/mode reset/tty restore after a write error. Adopting an id closed by this session resumes its last sent `main` and frame sequence; `dock`/`layer` start empty; unknown adopted id first deletes `main`.

**Recording** (`record.rs`): `TERN_TSP_RECORD=<file>` appends JSONL both ways; `Options::record` overrides.

**Environment honored**: `TERN_TSP=0` (never speak TSP), `TERN_TSP_RECORD`, `TMUX`/`STY`/`ZELLIJ` (no TSP), and `Session::connect` returns `None` when stdin/stdout isn't a tty or DA1 answers first/timeout.

### 11.4 How an integration is written

Quick start (`rust/README.md`; examples `rust/examples/{ask,chat,progress,table}.rs`):

```rust
use tern_sdk::{PrintOptions, ui, ui::View};
let view: View = View::new().main([ui::card().head("Deploy").status(ui::Status::Done)
    .child(ui::kv().item("service","api-gateway").item("region","eu-west-1"))]);
tern_sdk::print(view, PrintOptions::new())?;   // native in Tern, plain text elsewhere
```

```rust
let Some(mut session) = Session::<Msg>::connect(Options::new())? else { return Ok(()); };
let sf = session.open(SurfaceOptions::flow())?;
session.render(sf, ui::html::button("Deploy").on_click(Msg::Deploy))?;
while let Some(input) = session.next(None)? { match input {
    Input::Msg(Msg::Deploy, _) => break,
    Input::Key(key) if key.is("escape") => break,
    _ => {} } }
session.close()
```

Views/regions: a `View` has `main`/`dock`/`layer`; a node given for a region is the region root; a list is wrapped in a `col`. Ids derive: region root `main`, child `<parent>.<key>`, e.g. `dock.ed` for a child with `key="ed"`. `Doc::from_json` accepts the plugin wire form `{main?,dock?,layer?}` with region roots as nodes (not lists).

Helpers: `print(view, PrintOptions::new().css(…).fallback(…))` -> a `flow` surface with `listen:false`, its sheet, one frame, `x` keep; without TSP/stdout-not-tty writes `plain(view, cols)` or the fallback. `ask(view, AskOptions::new().css(…).submit("submit"))` returns `Answer::Submitted(Submission{id,act,values})`, `Answer::Cancelled` (Escape/Ctrl+C/Ctrl+D) or `Answer::Unsupported`; `ask_with(view,opts,|msg|…)` runs handlers while waiting. `plain(view, cols)` renders readable plain text.

Tests drive sessions over a scripted in-memory terminal implementing `term::Terminal` (`cargo test`); `tests/session.rs`.

---

## 12. Mapping ACP v1 onto TSP (for acp-tsp)

ACP v1 (https://agentclientprotocol.com/protocol/v1/overview, `/schema`): JSON-RPC 2.0 over stdio; **client** calls `initialize`, `authenticate`, `session/new`, `session/prompt`, optional `session/load`, `logout`, `session/set_mode`; client sends `session/cancel` notification; **agent** calls `session/request_permission`, optional `fs/read_text_file`, `fs/write_text_file`, `terminal/create|output|release|wait_for_exit|kill`, `elicitation/create`; agent sends `session/update` notifications (message chunks agent/user/thought, tool calls + updates, plans, available commands, mode changes) and `elicitation/complete`. Paths absolute, line numbers 1-based, camelCase keys, snake_case discriminators, `_meta` extensibility, `_`-prefixed custom methods.

Where each ACP concept lands in TSP:

| ACP | TSP carrier (proposed mapping) |
|---|---|
| Role: bridge is ACP **client**, spawns agent subprocess | Bridge owns stdio JSON-RPC to the agent itself. TSP has no subprocess/RPC — it is only the renderer channel |
| `initialize` / capabilities | TSP `hello` handshake is the *other* negotiation (bridge<->Tern). Two independent negotiations |
| `authenticate`, auth methods | **No TSP analogue.** Auth UI must be built in TSP (e.g. `el` form / `prefs` / `overlay`) and answered via ACP `authenticate` |
| `session/new` | One TSP surface per session (`o`, inline for interactive, or `flow` for background). Tern itself has no "session" concept |
| `session/prompt` | A user prompt arrives as pty keys or a `send` event into an `editor` composer; bridge translates Enter -> `session/prompt` |
| streaming agent message | `text`/`md` node + `text append` ops (streaming path; `md stream:true` for the provisional last block) |
| agent thought chunks | A `card`/`md`/`shimmer` node (omp-style "thinking"), or `md` with a muted style. TSP has no native "thinking" type |
| tool calls / tool_call_update | `tool` kind: `name`(icon)/`title`(verb)/`target`(+`targetKind`)/`status` pending-running-done-error-cancelled/`age`/`took`/`exit`/`note`/body children (`code`,`ansi`,`diff`,`table`). Updates via `set`/`text append` |
| `session/request_permission` | An `overlay` (modal) or `el` `form` with `actions` buttons, or `prefs`-style choice; answer carried by `action`/`change` events, incl. `values`. No protocol-level permission channel |
| `fs/read_text_file`, `fs/write_text_file` | **No TSP analogue** — bridge does FS access itself (security docs: TSP can't run host commands) |
| `terminal/create|output|kill|…` | `ansi` node + `code`; process management is the bridge's own; display via `tool`/`card`/`ansi`/`block` |
| plans | `checklist` kind (`phases`/`items` with status pending/active/done/dropped/blocked, `mode` full/hud/reminder) |
| available commands / slash commands | `picker` (searchable sheet) or `list`+`item`; no protocol-level command registry |
| mode changes / `session/set_mode` | `prefs` choice control, `seg`/`status` display, or a `picker`; no mode registry in TSP |
| models / usage / cost | `agent` stats (model chip, context meter, tokens, cost, timer) and `meter`/`rate` |
| cancellation `session/cancel` | Key (Escape/Ctrl+C) or an `actions.click`/`tool` button -> ACP notification; TSP `suspend` only hands the pane back |
| errors | TSP `error` events are transport/render errors, not ACP errors; ACP JSON-RPC errors must be rendered as `text`/`card tone:error`/`toast tone:error` |
| turn end (`session/prompt` response + stop reason) | **No TSP signal.** Bridge decides UI (mark `tool`/`card` done, set `status`, stop shimmer). Tern never tells a program a "turn" ended |
| elicitation | `el` form / `picker` / `overlay`; `elicitation/complete` -> update node |
| `_meta`, custom methods | No direct analogue; TSP has `role`/`key`/`props` and unknown-field tolerance |

### 12.1 Two handshakes, two lifecycles (important)

`[INFERENCE]` acp-tsp needs **two independent state machines**:

1. **TSP link to Tern**: probe `hello`+DA1 (or start-without-waiting when `TERM_PROGRAM=tern`), keep `credits`/`apc`/`kinds`, render views, route `event`s to node ids, ack-driven coalescing. If no Tern (DA1 first / timeout / `TERN_TSP=0` / tmux), fall back to ANSI or a plain REPL.
2. **ACP link to the agent**: spawn the agent subprocess over stdio JSON-RPC, do `initialize`/`authenticate`/`session/new`, correlate request ids, stream `session/update` into TSP ops, answer `session/request_permission` and agent->client calls.

The bridge is a client in *both* directions (client of Tern's rendering, client of the agent). Tern is the only server-ish peer, and only for `hello`/`blobs` replies.

---

## 13. Gaps: TSP features with no ACP analogue, and vice versa

### 13.1 TSP has, ACP lacks

- The whole **rendering/document model**: node kinds, frames/ops, ids, regions (`main`/`dock`/`layer`), styling, palettes, stylesheets, icons, motion, virtualized long content.
- **Credits / flow control** and **settle/retention/gone**, snapshots/persistence, blobs (content-addressed images), `rows` ANSI fallback.
- **Terminal input as pty keys** (kitty protocol, bracketed paste), native editing (`edit`/`undo`), `send` (host-injected prompts), IME.
- **Surface lifecycle**: inline/screen/flow modes, anchor rows, adopt, suspend/resume, listen:false, screen-over-inline, crash/prompt semantics.
- **UI interaction events** (`toggle`/`select`/`activate`/`action`/`change`/`focus` and form `values`), pointer/action model, context menus, copy/open/zoom.
- **Visibility/theme/motion signals** (`visible`, `theme`, `motion`), `hour12`, cell size, `cols`.
- Domain UI kinds ACP has no notion of: `prefs`, `checklist` modes, `agent` stats, `tool` presentation, `picker`, `meter`/`chart`/`effort`, `kv`/`table`/`tree`/`badge`/`kbd`/`icon`, `toast`, `overlay`, `el` forms.

### 13.2 ACP has, TSP lacks (bridge must implement itself)

- **JSON-RPC request/response semantics**, ids, errors, notifications — TSP is one-way.
- **Sessions as protocol objects** (`sessionId`, `session/new`, `session/load`, `session/close`) — TSP surfaces are the only handle.
- **Prompts/turns and stop reasons** — no turn lifecycle; no completion signal to render.
- **Permissions/approvals protocol** (`session/request_permission`) — only UI events.
- **File system ops** (`fs/*`) — TSP cannot read/write host files for you; and it can't run commands.
- **Terminal/process management** (`terminal/*`) — TSP has `ansi`/`code` for display, nothing for execution.
- **Auth** (`authenticate`, auth methods, `logout`).
- **Capability negotiation with the agent** (client/agent capabilities, MCP capability, `loadSession`, prompt capabilities image/audio/embeddedContext).
- **Session modes** (`session/set_mode`), **slash commands registry** (available commands update), **plans as structured `Plan`** — TSP only has display kinds.
- **Structured tool-call payloads/status/content blocks** (content types: text/image/audio/resource links, embedded context) — TSP represents them as nodes/blobs; mapping is lossy and must be designed.
- **Model selection / usage / cost as protocol fields** — TSP only renders them (`agent` stats).
- **MCP servers** (`mcpCapabilities`), elicitation protocol shape.

### 13.3 Tension points for the bridge

- TSP capability detection ≠ ACP: the bridge must handle "no Tern" (fall back to ANSI) *and* "no agent".
- `send` requires `hello.features` include `send` and a `sendable:true` field; Tern may inject prompts (Carly/initial prompt) which must be routed to `session/prompt` rather than treated as typed keys.
- `edit`/`undo` give Tern a native selection over the composer text; the bridge must own the text model and answer with `text`/`splice` + `cursor`, or accept `edit`/`undo` events (turn on `edit`+`undo` in the hello features).
- Nothing in TSP ends a turn: mark `tool`/`card` done, drop `shimmer`, set `status`, and remove the working indicator explicitly.
- Streaming UX is `text append` on `md`/`ansi`/`text` (cheap) or `set`+`text replace`; choose node ids stably for tool cards so updates are `set`/`text` not `del`+`add`.
- Only 2 unacked frames: coalesce rapid `session/update` chunks into pending op lists (the SDK's `render` already does view-diff queuing; raw streaming append may need `send` with queueing).
- Events reach the bridge only while a listening surface is open; a backgrounded/orphaned surface must use `listen:false` (`print`-style) or expect silence.
- Tern may drop settled nodes (`gone`) under retention; don't keep referencing dropped ids.
- `screen` surfaces don't persist across daemon restart; inline/flow come back read-only, so re-render after reattach (snapshot replay does not re-deliver events).

---

## 14. Quick reference: wire constants and enums (SDK)

```rust
pub const VERSION: u32 = 1;
pub const APC_LIMIT: usize = 65_536;
pub const CREDITS: u32 = 2;
pub const MAX_BLOB: usize = 16 << 20;

pub enum Mode { Inline, Screen, Flow }               // "inline"|"screen"|"flow"
pub enum TextMode { Append, Replace }
pub enum RevealAt { Start, End, Nearest }
pub enum ScrollBy { LineUp, LineDown, PageUp, PageDown, Start, End }  // kebab-case
pub enum Reply { Hello(Hello), Blobs(BlobsReply), Unknown(Map) }
pub enum Event { Ack, Resize, Theme, Motion, Visible, Toggle, Select, Activate,
                 Action, Change, Focus, Edit, Undo, Send, Error, Gone, Unknown }  
// Unknown kept whole; Event::name()/raw()/sf()/id(); Reply::raw()
```

Kind constants: `wire::kind::{COL,ROW,CARD,SECTION,RULE,SPACER,TEXT,MD,CODE,DIFF,ANSI,ROWS,MATH,KV,TABLE,TREE,BADGE,KBD,ICON,IMAGE,LIST,ITEM,TABS,PICKER,SPINNER,SHIMMER,ELAPSED,RATE,PROGRESS,METER,CHART,EFFORT,EDITOR,INPUT,STATUS,SEG,TOAST,OVERLAY,TOOL,AGENT,CHECKLIST,BLOCK,PREFS,EL}` (44), plus `ALL`, `TEXT_KINDS`, `has_text(kind)`.
Feature constants: `wire::feature::{EDIT,UNDO,SEND}` (program) and `{BLOBS,SETTLE,ADOPT,DOCK,PROGRAM_PALETTE,REDUCE_MOTION,ASIDE,SCROLL,STYLES,FLOW}` (terminal).

Typed UI enums (`ui::types`): `Tone`, `Status`, `AgentStatus`, `Mark`, `Gap`, `Align`, `Justify`, `Wrap`, `Truncate`, `TextAlign`, `Fx`, `Token`, `SpinnerStyle`, `ShimmerMode`, `ElapsedFormat`, `MeterStyle`, `Size`, `ChartKind`, `EffortLevel`, `DiffMode`, `KvLayout`, `Measure`, `CardVariant`, `PickerSize/Layout/Preview/Focus/State`, `TargetKind`, `ToolFrame`, `ChecklistMode`, `ItemStatus`, `OverlaySize`, `Side`, `ColumnFormat`, `ChoiceStyle`, `InputType` — each a `string_enum` with an `Other(String)` escape hatch, `as_str()`, `From<&str>`, `Display`, `Serialize` as a string. Plus `Span` (`{t,s?,fx?,href?}`), `Text` (Plain|Spans), `Extent` (Ch|Lines|Fraction), `Bound {w,h}`, `Basis` (Fraction|Content), `NodeKey` (Str|Int), `Actions {click,dblclick,menu}`.

---

## 15. Bottom line for acp-tsp

- TSP gives the bridge a **native agent UI** vocabulary (`tool`, `agent`, `checklist`, `md`, `code`, `diff`, `ansi`, `editor`, `prefs`, `picker`, `overlay`, `el`) with a credit-based streaming model and an events-only reverse channel. Everything ACP calls a "method" (prompt, permission, fs, terminal) has **no TSP message**: the bridge implements ACP as an in-process client and expresses its *effects* as TSP nodes/ops and its *inputs* as routed TSP events.
- The bridge must be tolerant: probe TSP, degrade per `hello.kinds`/`features`, and fall back to ANSI when Tern is absent (SDK `Session::connect` -> `None`; `print`/`ask`/`plain` already model this).
- The Rust SDK is small, sync, serde-only, edition 2024, no tokio, published as `tern-sdk` 0.1.0; its `Session<M>` + typed builders + `reconcile::Doc` diffing + event routing map cleanly onto an event-loop bridge (poll `Session::next`, translate ACP `session/update` into `render`/`send`), but a bridge that also runs an agent subprocess will need its own threads/async for the JSON-RPC side (the SDK provides none).
- Watch: only 2 unacked frames; no turn-end signal; `gone`/retention; `listen:false` silence; `send`/`edit`/`undo` are opt-in features; no auth, no fs, no command execution in TSP.
