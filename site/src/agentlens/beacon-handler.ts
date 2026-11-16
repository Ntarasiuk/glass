import { refineWithBeacon, type BeaconSignals } from "./classify.ts";
import { verifyToken, visitorId } from "./crypto.ts";
import { aiSource, hostOf } from "./referrer.ts";
import { packClient, parseClient } from "./ua.ts";
import { geoOf, logErr, projectOf, write, type AgentLensEnv, type ObserveOptions } from "./shared.ts";

const MAX_BEACON_BYTES = 8 * 1024;

interface BeaconBody {
  t?: string;
  k?: string;
  p?: string;
  r?: string;
  u?: string;
  m?: string;
  c?: string;
  w?: number;
  tp?: number;
  s?: unknown;
  n?: string;
  pr?: unknown;
  v?: number;
  d?: number;
  mv?: number;
  sc?: number;
  ky?: number;
  ck?: number;
  cnm?: number;
  tc?: number;
  ut?: number;
  st?: number;
  sd?: number;
  lcp?: number;
  inp?: number;
  cls?: number;
  ttfb?: number;
}

const num = (x: unknown) => (typeof x === "number" && Number.isFinite(x) ? x : 0);
const str = (x: unknown, max: number) => (typeof x === "string" ? x.slice(0, max) : "");

/** POST /_al/e from the inlined beacon: pv, engage, event. Token binds it to the edge verdict + host. */
export async function handleBeacon(req: Request, env: AgentLensEnv, ctx: ExecutionContext, opts: ObserveOptions): Promise<Response> {
  if (req.method !== "POST") return new Response(null, { status: 405 });
  if (Number(req.headers.get("content-length") ?? 0) > MAX_BEACON_BYTES) return new Response(null, { status: 413 });

  const raw = await readLimited(req, MAX_BEACON_BYTES);
  if (raw === null) return new Response(null, { status: 413 });
  let b: BeaconBody;
  try {
    b = JSON.parse(raw) as BeaconBody;
  } catch {
    return new Response(null, { status: 400 });
  }
  const url = new URL(req.url);
  const host = projectOf(url.hostname);
  const edgeCls = b.t ? await verifyToken(env.AL_SECRET ?? "", host, b.t) : null;
  if (!edgeCls) return new Response(null, { status: 403 });
  const kind = b.k === "pv" || b.k === "engage" || b.k === "event" ? b.k : null;
  if (!kind) return new Response(null, { status: 400 });

  const ua = req.headers.get("user-agent") ?? "";
  const ip = req.headers.get("cf-connecting-ip") ?? "";
  const sig: BeaconSignals = {
    static: Array.isArray(b.s) ? b.s.filter((x): x is string => typeof x === "string").slice(0, 16).map((x) => x.slice(0, 40)) : [],
    dwellMs: num(b.d),
    mouseMoves: num(b.mv),
    scrolls: num(b.sc),
    keys: num(b.ky),
    clicks: num(b.ck),
    clicksWithoutMove: num(b.cnm),
    touches: num(b.tc),
    untrusted: num(b.ut),
    straightness: num(b.st),
  };
  const refined = refineWithBeacon(edgeCls, sig);
  const refHost = hostOf(str(b.r, 2048));
  const external = refHost !== "" && projectOf(refHost) !== host;
  const utm = str(b.u, 64);
  const path = str(b.p, 512);
  const cf = req.cf ?? {};
  const isHuman = refined.cls === "human" || refined.cls === "agentic_browser";

  ctx.waitUntil(
    (async () =>
      write(env, {
        index: host,
        source: "beacon",
        kind,
        cls: refined.cls,
        edge_cls: edgeCls,
        verification: "none",
        client: packClient(parseClient(ua, num(b.tp), !isHuman)),
        path: opts.redactPath ? opts.redactPath(path) : path,
        referrer_host: external ? refHost : "",
        ai_source: aiSource(external ? refHost : "", utm),
        utm_source: utm,
        utm_medium: str(b.m, 64),
        ...geoOf(cf),
        asn_org: String(cf.asOrganization ?? ""),
        ua,
        signals: refined.signals.join(","),
        event_name: kind === "event" ? str(b.n, 64) : "",
        visitor: await visitorId(env.AL_SECRET ?? "", host, ip, ua),
        detail: kind === "event" ? (b.pr ? JSON.stringify(b.pr).slice(0, 1024) : "") : kind === "pv" ? str(b.c, 128) : "",
        duration_ms: sig.dwellMs,
        confidence: refined.confidence,
        asn: Number(cf.asn ?? 0),
        interactions: (sig.mouseMoves ?? 0) + (sig.scrolls ?? 0) + (sig.keys ?? 0) + (sig.clicks ?? 0) + (sig.touches ?? 0),
        value: num(b.v),
        cf_bot_score: -1,
        bytes: -1,
        is_entry: kind === "pv" && (external || !refHost) ? 1 : 0,
        viewport: Math.max(0, Math.min(10000, Math.round(num(b.w)))),
        ...(kind === "engage"
          ? {
              scroll: Math.max(0, Math.min(100, Math.round(num(b.sd)))),
              lcp: Math.max(0, Math.min(120_000, Math.round(num(b.lcp)))),
              inp: Math.max(0, Math.min(60_000, Math.round(num(b.inp)))),
              layout_shift: Math.max(0, Math.min(100_000, Math.round(num(b.cls)))),
              ttfb: Math.max(0, Math.min(120_000, Math.round(num(b.ttfb)))),
            }
          : {}),
      }))().catch(logErr("record_beacon_failed")),
  );
  return new Response(null, { status: 204 });
}

async function readLimited(req: Request, max: number): Promise<string | null> {
  if (!req.body) return "";
  const reader = req.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > max) {
      await reader.cancel();
      return null;
    }
    chunks.push(value);
  }
  const buf = new Uint8Array(size);
  let off = 0;
  for (const c of chunks) {
    buf.set(c, off);
    off += c.byteLength;
  }
  return new TextDecoder().decode(buf);
}
