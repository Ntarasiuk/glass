# Self-hosting remote access

`glass link --anywhere` normally goes through glassdesk.dev, which only introduces the
browser to your computer. Everything after that is end-to-end encrypted, and the host
signs its answers, so glassdesk.dev can't impersonate it. To run the introducer yourself
anyway, this folder has:

- **glass**: the same Worker glassdesk.dev runs (introducer, web client at `/app/`),
  served by wrangler's local runtime (workerd). No Cloudflare account needed. State goes
  in a volume.
- **caddy**: HTTPS with an automatic Let's Encrypt certificate.
- **coturn**: a TURN relay for viewers whose networks block direct connections.

## Steps

You need a server with a public IP, a DNS name pointing at it, and Docker. Open TCP 80
and 443, TCP+UDP 3478, and UDP 49160–49200.

```sh
git clone https://github.com/Ntarasiuk/glass && cd glass/site/selfhost
cp .env.example .env
# edit .env: GLASS_DOMAIN=glass.example.com, TURN_SECRET=$(openssl rand -hex 32)
docker compose up -d --build
curl -fsS https://glass.example.com/version.json   # check that it's up
```

Then, on each computer you want to reach:

```sh
glass config relay https://glass.example.com
glass link --anywhere
```

Links now open `https://glass.example.com/app/…`, and the host keeps its signalling
connection there instead of glassdesk.dev. `glass config relay ""` switches back.

## Notes

- Updates and `glass feedback` still use glassdesk.dev. Only remote access moves.
- Without TURN (if you remove the coturn service and its env), most connections still
  work directly. Viewers on strict corporate or mobile networks may not connect.
- coturn refuses to relay into private address ranges, so the relay can't be used to
  reach your LAN.
- Updating: `git pull && docker compose up -d --build`.
