#!/bin/bash
# Fast mode for Apple Silicon: run the embedding server natively on the Metal GPU (Docker on a Mac is CPU-only).
#   ./mac-native.sh install    create ~/.docask-embed/venv, start at login (launchd), listen on 127.0.0.1:8081
#   ./mac-native.sh status     launchd state + /health
#   ./mac-native.sh logs       follow the log
#   ./mac-native.sh uninstall  stop and remove the launchd job (venv and model cache are kept)
# Settings (EMBED_TOKEN, EMBED_*) come from .env next to this script, same as Docker.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HOME_DIR="$HOME/.docask-embed"
VENV="$HOME_DIR/venv"
LABEL="com.docask.embed"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
PORT="${EMBED_NATIVE_PORT:-8081}"
LOG="$HOME_DIR/server.log"

run() {  # launchd entry point: load .env, then serve
  set -a; . "$HERE/.env"; set +a
  export EMBED_DEVICE="${EMBED_DEVICE:-mps}" HF_HUB_DISABLE_TELEMETRY=1 PYTORCH_ENABLE_MPS_FALLBACK=1
  exec "$VENV/bin/uvicorn" app:app --app-dir "$HERE" --host 127.0.0.1 --port "$PORT" --no-access-log
}

install() {
  [ "$(uname -s)/$(uname -m)" = "Darwin/arm64" ] || { echo "needs macOS on Apple Silicon"; exit 1; }
  grep -q '^EMBED_TOKEN=.\{16,\}' "$HERE/.env" 2>/dev/null || { echo "set EMBED_TOKEN in $HERE/.env first"; exit 1; }
  mkdir -p "$HOME_DIR" "$(dirname "$PLIST")"
  UV="$(command -v uv || true)"
  if [ -z "$UV" ]; then
    UV="$(python3 -m site --user-base)/bin/uv"
    [ -x "$UV" ] || python3 -m pip install --user -q uv
  fi
  [ -x "$VENV/bin/python" ] || "$UV" venv -q --python 3.12 "$VENV"
  # every install: picks up libraries added by updates (fast when nothing changed)
  VIRTUAL_ENV="$VENV" "$UV" pip install -q torch==2.5.1 -r "$HERE/requirements.txt"
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>/bin/bash</string><string>$HERE/mac-native.sh</string><string>run</string></array>
  <key>EnvironmentVariables</key><dict><key>EMBED_NATIVE_PORT</key><string>$PORT</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict></plist>
EOF
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  # bootout returns before the old server has exited; bootstrapping too early fails with "5: Input/output error"
  for _ in $(seq 1 30); do launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || break; sleep 1; done
  launchctl bootstrap "gui/$(id -u)" "$PLIST"
  echo "started; first start loads the model (downloads ~2.3 GB if not cached). Check: $0 status"
}

case "${1:-}" in
  run) run ;;
  install) install ;;
  uninstall) launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true; rm -f "$PLIST"; echo "removed" ;;
  status) launchctl print "gui/$(id -u)/$LABEL" 2>/dev/null | grep -E '^\s+(state|pid) =' || echo "not installed"
          curl -s --max-time 5 "http://127.0.0.1:$PORT/health" || echo "no answer on :$PORT"; echo ;;
  logs) tail -f "$LOG" ;;
  *) sed -n '2,7p' "$0"; exit 1 ;;
esac
