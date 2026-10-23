# The glassdesk.dev Worker (introducer for remote access, web client at /app/), run by
# wrangler's local runtime (workerd) so it needs no Cloudflare account. Build from the
# repository root: docker build -f site/selfhost/Dockerfile .
FROM node:22-bookworm-slim

ENV WRANGLER_SEND_METRICS=false CI=1
# workerd needs system CA certificates to start (and to fetch Cloudflare TURN, if used).
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*
WORKDIR /glass
# build.mjs reads the version, the agent skill and the web client from the repo.
COPY Cargo.toml ./
COPY skills ./skills
COPY web ./web
COPY site ./site
WORKDIR /glass/site
RUN npm ci --no-audit --no-fund && node build.mjs
COPY site/selfhost/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 8787
VOLUME /data
ENTRYPOINT ["/entrypoint.sh"]
