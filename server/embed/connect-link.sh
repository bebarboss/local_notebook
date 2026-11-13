#!/bin/bash
# Print the DocAsk setup link for this server: docask://connect?url=…&token=…
# Send it to the team (chat / e-mail). Clicking it opens DocAsk, which asks once and then
# connects by itself — nobody types the token. Whoever has the link can use the server:
# share it only inside the team; to revoke it, change EMBED_TOKEN in .env and make a new link.
#   ./connect-link.sh                 URL of the Cloudflare tunnel (from its log)
#   ./connect-link.sh https://host    another URL (Named Tunnel, Tailscale …)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOKEN="$(grep -E '^EMBED_TOKEN=' "$HERE/.env" | tail -1 | cut -d= -f2- | tr -d '"'"'"' \r')"
[ "${#TOKEN}" -ge 16 ] || { echo "EMBED_TOKEN in $HERE/.env is missing or too short" >&2; exit 1; }

URL="${1:-}"
if [ -z "$URL" ]; then
  URL="$(cd "$HERE" && docker compose logs tunnel 2>/dev/null | grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' | tail -1 || true)"
  [ -n "$URL" ] || { echo "no tunnel URL in the log — is 'docker compose up -d' running? or pass the URL: $0 https://…" >&2; exit 1; }
fi
curl -fsS --max-time 10 "$URL/health" >/dev/null || echo "warning: $URL/health does not answer right now" >&2

enc() {  # percent-encode everything but unreserved characters
  local s="$1" out="" c i
  for ((i = 0; i < ${#s}; i++)); do
    c="${s:i:1}"
    case "$c" in [a-zA-Z0-9.~_-]) out+="$c" ;; *) out+="$(printf '%%%02X' "'$c")" ;; esac
  done
  printf '%s' "$out"
}

echo "docask://connect?url=$(enc "$URL")&token=$(enc "$TOKEN")"
