# Design: an omp-style UI

Goal: acp-tsp should look and feel like omp (oh-my-pi) in Tern, not like a generic chat.

## Key finding

omp sends **no stylesheet**. Its look comes from CSS that **Tern ships built in**, keyed on the common `role` prop (`[data-role='omp.user']`, `.sf-card[data-role^='omp.tool']`, …). These rules are not scoped to omp (`data-app`/`data-surface` are not part of the selectors), so any program that uses omp's role vocabulary and node structure gets omp's look. Tern also lays the main region out as a centred "chat reader" column.

Decision: acp-tsp adopts omp's role vocabulary and node shapes verbatim. The risk is coupling to Tern's omp CSS. We accept it because it is the only styling Tern provides for agent UIs, and Tern ships it with the terminal itself. Each surface element documents which omp roles it relies on, so a Tern change is easy to trace.

We do not reuse omp branding: no `image builtin:"omp"` logo and no omp tips. The welcome screen shows the ACP agent's own title and version.

## Reference material (local; not committed)

| What | Where |
|---|---|
| omp TSP recording of one turn plus the `/` and `@` autocompletes and the `/model` picker (headless Tern 0.6.2, omp 18.8.5) | `/tmp/omp-ref/omp-tsp.jsonl`; pretty-printed in `/tmp/omp-ref/omp-frames.txt` |
| Tern's built-in omp CSS (extracted with `strings`, so lines of fewer than 4 chars such as `}` are missing) | `/tmp/omp-ref/css/omp-all.css` (`omp.css`, `omp-pickers.css`, `omp-prefs.css`, `omp-transcript.css`, `omp-tools.css`, `omp-panels.css`, `omp-chat-*.css`) |
| Screenshots of omp | `/tmp/omp-ref/target/shots/tern/serve/omp-*.png` (`welcome`, `working`, `turn`, `shell`, `slash`, `slash-mo`, `at`, `model`) |
| Screenshots of current acp-tsp | `/tmp/e2e2/target/shots/tern/serve/*.png` |
| omp source (MIT): each component's `describe()` builds its TSP node | `~/.bun/install/global/node_modules/@oh-my-pi/pi-tui/src/` — `chat/{user-message,assistant-message,compaction-summary-message,todo-reminder,ttsr-notification}.ts`, `prompt/{custom-editor,welcome}.ts`, `components/{loader,editor}.ts` (working row, autocomplete), `tools/*.ts` (per-tool cards: `read`, `edit`, `write`, `bash-interactive`, `find`, `glob`, `task`, `ask`, …), `overlays/{ask-dialog,plan-review-overlay,error-banner}.ts`, `native/{describe,picker,overlay,icons,tone,spans}.ts`; `pi-coding-agent/src/modes/interactive-mode.ts` (HUD: todo, jobs) |

## Mapping (from the recording)

- **Palette:** send a `t` palette right after `o`, like omp (frame 1: `name {dark:"titanium", light:"light"}`, token maps). Use the same token names so role CSS and span styles (`statusLineModel`, `toolDiffAdded`, `mdCode`, `thinkingText`, …) resolve.
- **Main region:** spacer `sm` items between entries. Welcome `card role=omp.welcome` holds a lockup row (`omp.welcome.lockup`: mark col with `omp.welcome.wordmark` strong text and `omp.welcome.version` dim mono) and an optional tip row.
- **User message:** `card role=omp.user tone=user` holding a `row role=omp.user.tools gap=xs` (time `text role=omp.user.time` dim mono with a full-date title; Copy and other actions are `text role=omp.user.tool` with `actions.click`), then an `md`. Rendered as a right-aligned bubble.
- **Assistant message:** `col role=omp.assistant` holding the `md` blocks. The turn footer is `row role=omp.turn.usage gap=xs align=center` with `icon name=time` and `text "3.5s · 484 tok · $0.01 this turn"` (detail in `title`).
- **Thinking:** see `assistant-message.ts` (roles `omp.thinking*`).
- **Tools:** `tool` kind with `role=omp.tool.<name>`, `name` (icon key), `title` (verb), `target`, `targetKind` (`path|command|pattern|query|text`), `href` (`file://`) for paths, `lang`, `status` `pending→running→done|error`, `age`/`took`, `frame` (`inline` for read-like tools, `card` for bash), `collapsible`/`collapsed`, `preview {tail:10}`. Bash output child is `ansi role=omp.tool.bash.output follow=false`. Settle finished tools.
- **Dock:** composer `col role=omp.editor` (tone `pending` while running) holding:
  - `meter role=omp.composer.context style=bar` with thresholds, label `N%`, `total` and `title`;
  - `row role=omp.composer.line` wrapping the `editor` (placeholder; `maxLines 18`);
  - `row role=omp.composer.bar` with:
    - the model chip `row role=omp.composer.model` (name `text` with `statusLineModel` span; `effort` node `role=omp.composer.model.effort`; `icon chev`; click opens the model picker);
    - `status role=omp.composer.extras transparent grow=1` holding `seg role=omp.composer.fact` items (icon plus spans, `priority`);
    - `text role=omp.composer.usage` (cost);
    - `kbd role=omp.composer.send keys=[enter]` (`text role=omp.composer.stop tone=error` while running).
- **Working row:** in the dock above the composer: `row role=omp.hud.activity` → `col role=omp.hud.status` → `row role=omp.working` with `spinner style=starburst tone=accent`, `elapsed format=short`, `text "·" role=omp.working.sep`, `shimmer role=omp.working.label` (mode `classic`, palette low/mid/high; label = current activity), a `grow` filler, and `row role=omp.working.stop` (`kbd escape` + "Stop", click → interrupt).
- **Autocomplete:** `/` and `@` open `omp.autocomplete`, an overlay anchored at the editor caret (see `components/editor.ts` and recording lines near `"role": "omp.autocomplete"`), not a full-screen picker. Rows show an icon, the name and a muted description.
- **Model and session pickers:** the `picker` kind with omp's props (recording around `"k": "picker"`, `native/picker.ts`).

## Acceptance

Side-by-side headless screenshots of the same scenarios (welcome, working, finished turn with read + bash tools, `/`, `@`, model picker, permission dialog, question form) are visually close to omp's. Interactions keep working. Stable node ids; settled finished entries.

## ACP rendering decisions

- The welcome mark is a neutral glyph, not the omp image; its title and version
  come from ACP `initialize` (the launch name is shown until it arrives).
- ACP tool kinds select omp-style cards: reads stay inline, shell commands have
  collapsible ANSI output and a tail preview, edits use TSP `diff`, and child
  calls nest under their parent. File paths resolve against the session cwd.
- A turn footer shows elapsed time and per-turn token/cost deltas when available.
  Context-window usage remains a **session** meter, never a token quota.
- The dock replaces the generic status strip with `omp.editor` and
  `omp.hud.activity` while working. Clipboard actions are omitted because
  tern-sdk has no clipboard action; a Copy button must not claim to work when
  no clipboard integration exists.
