#!/bin/sh
# Sets up the TURN relay that remote access falls back to when a direct connection isn't
# possible (some cellular and corporate networks). Creates a Cloudflare Realtime TURN key
# and stores it as secrets on the glassdesk.dev Worker. Secrets go straight from the API
# into `wrangler secret put` and are never printed.
#
#   scripts/setup-relay.sh
#
# Needs a Cloudflare API token with the "Calls Write" permission (dash.cloudflare.com ›
# My Profile › API Tokens › Create Token › Custom token › Account › Calls › Edit). Pass it
# as CLOUDFLARE_API_TOKEN, or paste it when asked. Re-running rotates the key: the new one
# is stored first, then older "glass-relay" keys are deleted.
set -eu
cd "$(dirname "$0")/../site"

NAME=glass-relay
API=https://api.cloudflare.com/client/v4

if [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
  printf 'Cloudflare API token (Calls Write): ' >&2
  stty -echo 2>/dev/null || true
  read -r CLOUDFLARE_API_TOKEN
  stty echo 2>/dev/null || true
  printf '\n' >&2
fi
[ -n "$CLOUDFLARE_API_TOKEN" ] || { echo "No token given." >&2; exit 1; }

api() { # api METHOD PATH [JSON]: prints the response, or Cloudflare's error and exits
  resp=$(curl -sS -X "$1" "$API$2" -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
    -H 'content-type: application/json' ${3:+-d "$3"}) || exit 1
  if ! printf '%s' "$resp" | node -e "let s='';process.stdin.on('data',c=>s+=c).on('end',()=>{let d;try{d=JSON.parse(s)}catch{d={}}if(d.success)process.exit(0);console.error('Cloudflare: '+((d.errors||[]).map(e=>e.message).join('; ')||s.slice(0,200)));process.exit(1)})"; then
    echo "($1 $2) Check the token has the Calls Write permission." >&2
    exit 1
  fi
  printf '%s' "$resp"
}
# json EXPR: evaluates a JS expression over the JSON on stdin (as `d`).
json() { node -e "let s='';process.stdin.on('data',c=>s+=c).on('end',()=>{const d=JSON.parse(s);const v=($1);if(v!==undefined)process.stdout.write(String(v))})"; }

if [ -z "${CLOUDFLARE_ACCOUNT_ID:-}" ]; then
  ACCOUNTS=$(api GET /accounts) || exit 1
  CLOUDFLARE_ACCOUNT_ID=$(printf '%s' "$ACCOUNTS" | json 'd.result.length===1?d.result[0].id:""')
fi
ACCOUNT=$CLOUDFLARE_ACCOUNT_ID
[ -n "$ACCOUNT" ] || { echo "Set CLOUDFLARE_ACCOUNT_ID (the token can see several accounts)." >&2; exit 1; }

KEYS=$(api GET "/accounts/$ACCOUNT/calls/turn_keys") || exit 1
OLD=$(printf '%s' "$KEYS" | json "d.result.filter(k=>k.name==='$NAME').map(k=>k.uid).join(' ')")

echo "Creating TURN key…" >&2
CREATED=$(api POST "/accounts/$ACCOUNT/calls/turn_keys" "{\"name\":\"$NAME\"}") || exit 1
printf '%s' "$CREATED" | json 'd.result.uid' | npx wrangler secret put TURN_KEY_ID >/dev/null
printf '%s' "$CREATED" | json 'd.result.key' | npx wrangler secret put TURN_KEY_API_TOKEN >/dev/null
CREATED=
echo "Stored on the Worker as TURN_KEY_ID and TURN_KEY_API_TOKEN." >&2

for uid in $OLD; do
  api DELETE "/accounts/$ACCOUNT/calls/turn_keys/$uid" >/dev/null && echo "Deleted the previous key $uid." >&2
done

# Check: glassdesk.dev now hands browsers relay servers.
BASE=${GLASS_BASE:-https://glassdesk.dev}
sleep 3
RELAYS=$(node -e "
const ws = new WebSocket('$(echo "$BASE" | sed 's/^http/ws/')/signal/connect/AAAAAAAAAAAAAAAAAAAAAA');
ws.onmessage = (e) => { console.log((JSON.parse(e.data).iceServers || []).length); process.exit(0); };
setTimeout(() => { console.log(0); process.exit(0); }, 10000);
")
if [ "$RELAYS" -gt 0 ]; then
  echo "Done: $BASE now offers the relay to browsers." >&2
else
  echo "The key is stored, but $BASE isn't offering a relay yet. Try again in a minute, or check the Worker's logs." >&2
  exit 1
fi
