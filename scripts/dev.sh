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

echo "==> Copying plugins into plugins.d/ ..."
mkdir -p "$ROOT/plugins.d"
for lib in "$ROOT"/engine/target/debug/lib*_plugin.*; do
  case "$lib" in
    *.dylib | *.so | *.dll) cp "$lib" "$ROOT/plugins.d/" ;;
  esac
done

echo "==> Starting engine on http://127.0.0.1:8080 ..."
"$ROOT/engine/target/debug/dvsim" serve --plugins "$ROOT/plugins.d" &
ENGINE_PID=$!

cleanup() {
  echo
  echo "==> Stopping engine (pid $ENGINE_PID) ..."
  kill "$ENGINE_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Install UI dependencies on first run.
if [ ! -d "$ROOT/ui/node_modules" ]; then
  echo "==> Installing UI dependencies ..."
  (cd "$ROOT/ui" && npm install)
fi

echo "==> Starting UI dev server on http://localhost:5173 ..."
cd "$ROOT/ui" && npm run dev
