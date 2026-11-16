import { classifyRequest } from "./classify.ts";
import { signToken, visitorId } from "./crypto.ts";
import { aiSource, hostOf } from "./referrer.ts";
import type { Verdict } from "./taxonomy.ts";
import { packClient, parseClient } from "./ua.ts";
import { BEACON_JS } from "./beacon.ts";
import { handleBeacon } from "./beacon-handler.ts";
import { verifyIdentity } from "./identity.ts";
import { geoOf, logErr, projectOf, write, type AgentLensEnv, type ObserveOptions } from "./shared.ts";

export type { AgentLensEnv, ObserveOptions };

type Next = (req: Request) => Promise<Response>;

const ASSET = /\.(?:js|mjs|css|png|jpe?g|gif|webp|avif|svg|ico|woff2?|ttf|otf|map|mp4|webm|mp3|wav)$/i;
const BEACON_PATH = "/_al/s.js";
const EVENT_PATH = "/_al/e";

/** Wrap any fetch handler: classify, pass through, inject beacon, record off the hot path. */
export async function observe(req: Request, env: AgentLensEnv, ctx: ExecutionContext, next: Next, opts: ObserveOptions = {}): Promise<Response> {
  // No secret = no salt for visitor hashes or token signing: stay out of the way entirely.
  if (!env.AL_SECRET) return next(req);
  const url = new URL(req.url);
  if (url.pathname === BEACON_PATH) return beaconScript();
  if (url.pathname === EVENT_PATH) return handleBeacon(req, env, ctx, opts);

  const started = Date.now();
  let verdict: Verdict | null = null;
  try {
    verdict = classifyRequest({
      ua: req.headers.get("user-agent") ?? "",
      headers: req.headers,
      asn: req.cf?.asn as number | undefined,
      cfBot: req.cf?.botManagement as { score?: number; verifiedBot?: boolean } | undefined,
    });
  } catch (err) {
    console.error(JSON.stringify({ msg: "classify_failed", err: String(err) }));
  }

  const res = await next(req);
  if (!verdict) return res; // analytics must never break the site

  const ms = Date.now() - started;
  const ctype = res.headers.get("content-type") ?? "";
  const isHtml = ctype.includes("text/html");
  const skip = verdict.cls === "human" && ASSET.test(url.pathname) && env.LOG_ASSETS !== "true";
  const v = verdict;
  if (!skip) ctx.waitUntil(recordEdge(req, env, opts, v, res, ms, isHtml, ctype).catch(logErr("record_edge_failed")));

  if (!isHtml || res.status !== 200 || env.INJECT === "false" || req.method !== "GET") return res;
  try {
    const token = await signToken(env.AL_SECRET, projectOf(url.hostname), verdict.cls);
    // Inline: no extra request (free-tier friendly, no render-blocking fetch) and nothing for ad-block lists to match.
    const tag = `<script data-t="${token}">${BEACON_JS}</script>`;
    // Body now differs per response: drop the origin ETag so browsers don't revalidate into an expired token.
    const out = new Response(res.body, res);
    out.headers.delete("etag");
    return new HTMLRewriter().on("head", { element: (el) => { el.append(tag, { html: true }); } }).transform(out);
  } catch (err) {
    console.error(JSON.stringify({ msg: "inject_failed", err: String(err) }));
    return res;
  }
}

function beaconScript(): Response {
  return new Response(BEACON_JS, {
    headers: {
      "content-type": "text/javascript; charset=utf-8",
      "cache-control": "public, max-age=3600, stale-while-revalidate=86400",
    },
  });
}

async function recordEdge(req: Request, env: AgentLensEnv, opts: ObserveOptions, v0: Verdict, res: Response, ms: number, isHtml: boolean, ctype: string) {
  const url = new URL(req.url);
  const ip = req.headers.get("cf-connecting-ip") ?? "";
  const ua = req.headers.get("user-agent") ?? "";
  const v = await verifyIdentity(req, v0, ip);
  const host = projectOf(url.hostname);
  const refHost = hostOf(req.headers.get("referer"));
  const external = refHost !== "" && projectOf(refHost) !== host;
  const utm = url.searchParams.get("utm_source") ?? "";
  const cf = req.cf ?? {};
  const human = v.cls === "human" || v.cls === "agentic_browser";
  write(env, {
    index: host,
    source: "edge",
    kind: "request",
    cls: v.cls,
    edge_cls: v.cls,
    agent: v.agent,
    operator: v.operator,
    verification: v.verification,
    client: packClient(parseClient(ua, 0, !human)),
    path: opts.redactPath ? opts.redactPath(url.pathname) : url.pathname,
    utm_medium: url.searchParams.get("utm_medium") ?? "",
    referrer_host: external ? refHost : "",
    ai_source: aiSource(external ? refHost : "", utm),
    utm_source: utm,
    ...geoOf(cf),
    asn_org: String(cf.asOrganization ?? ""),
    ua,
    signals: v.signals.join(","),
    visitor: await visitorId(env.AL_SECRET ?? "", host, ip, ua),
    detail: ctype.split(";")[0] ?? "",
    status: res.status,
    duration_ms: ms,
    confidence: v.confidence,
    asn: Number(cf.asn ?? 0),
    is_html: isHtml ? 1 : 0,
    cf_bot_score: Number((cf.botManagement as { score?: number } | undefined)?.score ?? -1),
    bytes: Number(res.headers.get("content-length") ?? -1),
    is_entry: isHtml && (external || !refHost) ? 1 : 0,
  });
}
