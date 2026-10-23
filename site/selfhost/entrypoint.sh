#!/bin/sh
# Applies the D1 migrations to the persisted local database, then serves the Worker.
set -eu
npx wrangler d1 migrations apply getglass --local --persist-to /data >/dev/null
set -- --ip 0.0.0.0 --port 8787 --persist-to /data --var "VISITOR_SALT:${VISITOR_SALT:-selfhost}"
# Your own TURN server (coturn, see docker-compose.yml) for viewers behind strict NATs.
if [ -n "${TURN_URLS:-}" ] && [ -n "${TURN_SECRET:-}" ]; then
  set -- "$@" --var "TURN_URLS:$TURN_URLS" --var "TURN_SECRET:$TURN_SECRET"
fi
exec npx wrangler dev "$@"
