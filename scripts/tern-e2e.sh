#!/usr/bin/env bash
# Drive acp-tsp inside a headless Tern (`tern serve`) for manual end-to-end checks.
#
#   scripts/tern-e2e.sh start [DIR]        start headless Tern (shell pane in DIR, default a temp dir)
#   scripts/tern-e2e.sh run ARGS...         type `acp-tsp ARGS...` + Enter in the focused pane
#   scripts/tern-e2e.sh type TEXT           type TEXT into the focused pane (the composer)
#   scripts/tern-e2e.sh key KEY             press KEY (Playwright names: Enter, Escape, 1, ...)
#   scripts/tern-e2e.sh text                print every text node of the window (surface included)
#   scripts/tern-e2e.sh stop                stop headless Tern
#
# Ctrl combinations are not delivered to the pty by the headless harness (Tern 0.6.2).
# Real agents need their own auth (claude/codex logins); nothing here mocks them.
set -euo pipefail
CTL=${TERN_E2E_CTL:-/tmp/acp-tsp-e2e.sock}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${ACP_TSP_BIN:-$ROOT/target/release/acp-tsp}
ctl() { tern ctl --control "$CTL" "$@"; }
quote() { printf '"%s"' "${1//\"/\\\"}"; }

case "${1:-}" in
  start)
    dir=${2:-$(mktemp -d)}
    (cd "$dir" && nohup tern serve --control "$CTL" >/tmp/acp-tsp-e2e.log 2>&1 &)
    for _ in $(seq 50); do ctl state >/dev/null 2>&1 && break; sleep 0.2; done
    ctl ready >/dev/null
    echo "headless Tern on $CTL in $dir"
    ;;
  run) shift; ctl type "$(quote "$BIN $*")" >/dev/null; ctl key Enter >/dev/null ;;
  type) ctl type "$(quote "$2")" >/dev/null ;;
  key) ctl key "$2" >/dev/null ;;
  text)
    ctl tree | python3 -c '
import json, sys
def walk(node):
    text = node.get("text")
    if text and node.get("class") != "tv-probe":
        print(text)
    for child in node.get("children", []):
        walk(child)
for node in json.load(sys.stdin)["tree"]:
    walk(node)'
    ;;
  stop) ctl quit >/dev/null ;;
  *) sed -n '2,13p' "$0"; exit 2 ;;
esac
