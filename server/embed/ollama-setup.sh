#!/bin/bash
# Ollama for the embedding server (bge-m3 on the Metal GPU), set up to survive a restart.
#   ./ollama-setup.sh install    pull bge-m3, install the login agent, restart Ollama with the settings below
#   ./ollama-setup.sh status     settings Ollama is running with + a test embedding
#   ./ollama-setup.sh uninstall  remove the login agent (Ollama itself and its models stay)
# `launchctl setenv` alone is forgotten at restart, so a LaunchAgent sets it again at every login and then
# (re)starts Ollama.app, which only reads the environment when it starts.
# Ollama listens on localhost only: Docker reaches it as host.docker.internal:11434 all the same.
set -euo pipefail
LABEL="com.docask.ollama"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
MODEL="${OLLAMA_EMBED_MODEL:-bge-m3}"
APP_DB="$HOME/Library/Application Support/Ollama/db.sqlite"

# runs at every login (launchd), and once at install
LOGIN_SCRIPT='launchctl setenv OLLAMA_KEEP_ALIVE -1
launchctl setenv OLLAMA_NUM_PARALLEL 2
launchctl unsetenv OLLAMA_HOST
pkill -TERM -x Ollama || true
for i in $(seq 1 30); do pgrep -qx Ollama || pgrep -qx ollama || break; sleep 1; done
open -g -a Ollama'

install() {
  command -v ollama >/dev/null && [ -d /Applications/Ollama.app ] || { echo "install Ollama first: https://ollama.com/download"; exit 1; }
  ollama pull "$MODEL"
  mkdir -p "$(dirname "$PLIST")"
  local s="${LOGIN_SCRIPT//&/&amp;}"; s="${s//</&lt;}"; s="${s//>/&gt;}"   # XML-escape for the plist
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>/bin/sh</string><string>-c</string><string>$s</string></array>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>/tmp/$LABEL.log</string>
  <key>StandardErrorPath</key><string>/tmp/$LABEL.log</string>
</dict></plist>
EOF
  plutil -lint -s "$PLIST"
  # the app's "Expose Ollama to the network" makes it listen on 0.0.0.0 whatever OLLAMA_HOST says
  if [ -f "$APP_DB" ] && [ "$(sqlite3 -readonly "$APP_DB" 'select expose from settings limit 1' 2>/dev/null)" = "1" ]; then
    # the app ignores AppleScript "quit" and keeps the setting in memory, so stop it before the change
    pkill -TERM -x Ollama || true
    for _ in $(seq 1 30); do pgrep -qx Ollama || pgrep -qx ollama || break; sleep 1; done
    sqlite3 "$APP_DB" 'update settings set expose = 0'
    echo "turned off \"Expose Ollama to the network\" (localhost only)"
  fi
  # launchd loads the agent by itself at every login; for now run the same steps directly (and wait for them)
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  /bin/sh -c "$LOGIN_SCRIPT"
  for _ in $(seq 1 60); do curl -sf --max-time 2 http://127.0.0.1:11434/api/version >/dev/null && break; sleep 1; done
  status
}

status() {
  for v in OLLAMA_KEEP_ALIVE OLLAMA_NUM_PARALLEL OLLAMA_HOST; do echo "launchctl $v=$(launchctl getenv $v)"; done
  echo "listening: $(lsof -nP -iTCP:11434 -sTCP:LISTEN 2>/dev/null | awk 'NR>1{print $9}' | sort -u | tr '\n' ' ')"
  curl -s --max-time 60 http://127.0.0.1:11434/api/embed -d "{\"model\":\"$MODEL\",\"input\":[\"ทดสอบ\"]}" |
    python3 -c 'import json,sys; d=json.load(sys.stdin); v=d["embeddings"][0]; print("embed ok:", len(v), "dims")' ||
    echo "embed FAILED"
  curl -s --max-time 5 http://127.0.0.1:11434/api/ps |
    python3 -c 'import json,sys; [print("loaded:", m["name"], "until", m["expires_at"][:10]) for m in json.load(sys.stdin)["models"]]'
}

case "${1:-}" in
  install) install ;;
  status) status ;;
  uninstall) launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true; rm -f "$PLIST"; echo "removed (run: launchctl unsetenv OLLAMA_KEEP_ALIVE OLLAMA_NUM_PARALLEL)" ;;
  *) sed -n '2,8p' "$0"; exit 1 ;;
esac
