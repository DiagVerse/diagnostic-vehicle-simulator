#!/usr/bin/env bash
#
# Development loop: build the engine, load its plugins, and run the engine + UI together.
#
#   ./scripts/dev.sh
#
# The engine serves the API on http://127.0.0.1:8080 and the Vite dev server serves the UI on
# http://localhost:5173 (proxying /health, /plugins, /ecu to the engine). Press Ctrl-C to stop
# both.
#
# Both run in the background and are watched, because the halves are useless apart and the
# failure is silent: the UI holds the page, the engine holds the data, and the browser talks to
# the engine *through* the UI's proxy. Lose the UI server and a tab that is still on screen has
# no backend for anything — no traffic, no buttons — while the engine sits there answering
# nobody. That reads as the app hanging. So if either stops, this script stops the other and
# says which went.
#
# Workflow after code changes:
#   - UI changes (ui/src): hot-reload automatically, just refresh the browser.
#   - Engine changes (engine/): re-run this script to rebuild and restart the engine.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Make cargo available in a non-login shell.
if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

echo "==> Building engine..."
(cd "$ROOT/engine" && cargo build)

BUILD_COMMIT="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
if [ -n "$(git -C "$ROOT" status --porcelain 2>/dev/null)" ]; then
  BUILD_COMMIT="${BUILD_COMMIT}+"
fi
echo "==> Built from commit ${BUILD_COMMIT}"

# Every plugin, not just the one being worked on. A dylib built against an older
# plugin-contract takes the engine down at startup with SIGKILL and no message at all.
echo "==> Copying plugins into plugins.d/ ..."
mkdir -p "$ROOT/plugins.d"
for lib in "$ROOT"/engine/target/debug/lib*_plugin.*; do
  case "$lib" in
    *.dylib | *.so | *.dll) cp "$lib" "$ROOT/plugins.d/" ;;
  esac
done

# Free the engine port if a previous run (or a stray process) is still holding it, so the
# engine can always bind. Only touches the fixed engine port; Vite picks its own port.
ENGINE_PORT=8080
if lsof -ti "tcp:${ENGINE_PORT}" >/dev/null 2>&1; then
  echo "==> Port ${ENGINE_PORT} is in use; stopping the previous listener ..."
  lsof -ti "tcp:${ENGINE_PORT}" | xargs kill 2>/dev/null || true
  sleep 1
fi

echo "==> Starting engine on http://127.0.0.1:${ENGINE_PORT} ..."
"$ROOT/engine/target/debug/dvsim" serve --addr "127.0.0.1:${ENGINE_PORT}" --plugins "$ROOT/plugins.d" &
ENGINE_PID=$!

# Install UI dependencies on first run.
if [ ! -d "$ROOT/ui/node_modules" ]; then
  echo "==> Installing UI dependencies ..."
  (cd "$ROOT/ui" && npm install)
fi

echo "==> Starting UI dev server on http://localhost:5173 ..."
(cd "$ROOT/ui" && npm run dev) &
UI_PID=$!

# `npm run dev` is a wrapper around vite, so the process to signal is not always the one whose
# pid we hold. Children are stopped first, then the wrapper, or vite outlives the script and
# keeps the port — which makes the next run fail for a reason that has nothing to do with it.
StopProcess() {
  strName=$1
  iPid=$2
  if kill -0 "$iPid" 2>/dev/null; then
    echo "==> Stopping ${strName} (pid ${iPid}) ..."
    pkill -TERM -P "$iPid" 2>/dev/null || true
    kill -TERM "$iPid" 2>/dev/null || true
  fi
}

cleanup() {
  echo
  StopProcess "UI dev server" "$UI_PID"
  StopProcess "engine" "$ENGINE_PID"
}
# HUP as well as INT and TERM: closing the terminal used to leave the engine orphaned, holding
# port 8080 against the next run.
trap cleanup EXIT INT TERM HUP

echo
echo "==> Engine  http://127.0.0.1:${ENGINE_PORT}   (commit ${BUILD_COMMIT})"
echo "==> UI      http://localhost:5173"
echo "==> Ctrl-C stops both. Watching ..."

# Poll rather than `wait -n`, which needs a bash newer than the one macOS ships as /bin/bash.
while true; do
  if ! kill -0 "$ENGINE_PID" 2>/dev/null; then
    echo
    echo "!!! The engine stopped. The UI cannot reach anything without it, so stopping that too."
    break
  fi
  if ! kill -0 "$UI_PID" 2>/dev/null; then
    echo
    echo "!!! The UI dev server stopped. Any browser tab still open is now talking to nothing —"
    echo "!!! it will look like the app has hung. Stopping the engine too; re-run this script."
    break
  fi
  sleep 1
done
