import { classifyRequest } from "./classify.ts";
import { visitorId } from "./crypto.ts";
import { aiSource, hostOf } from "./referrer.ts";
import { packClient, parseClient } from "./ua.ts";
import { verifyIdentity } from "./identity.ts";
import { geoOf, logErr, projectOf, write, type AgentLensEnv, type ObserveOptions } from "./shared.ts";

/**
 * Record a goal that happens on the server, with no browser involved: `curl … | sh` installs,
 * file downloads, API signups. The row carries the requester's class (human, agent, script…),
 * verified identity, location and device, so the dashboard can split conversions by who made them.
 * Runs entirely in waitUntil; never throws into the caller.
 */
export function trackServerEvent(
  req: Request,
  env: AgentLensEnv,
  ctx: ExecutionContext,
  name: string,
  props?: Record<string, string | number | boolean>,
  value = 0,
  opts: ObserveOptions = {},
): void {
  if (!env.AL_SECRET) return;
  const secret = env.AL_SECRET;
  ctx.waitUntil(
    (async () => {
      const url = new URL(req.url);
      const ua = req.headers.get("user-agent") ?? "";
      const ip = req.headers.get("cf-connecting-ip") ?? "";
      const v = await verifyIdentity(
        req,
        classifyRequest({ ua, headers: req.headers, asn: req.cf?.asn as number | undefined, cfBot: req.cf?.botManagement as { score?: number } | undefined }),
        ip,
      );
      const host = projectOf(url.hostname);
      const refHost = hostOf(req.headers.get("referer"));
      const external = refHost !== "" && projectOf(refHost) !== host;
      const utm = url.searchParams.get("utm_source") ?? "";
      const cf = req.cf ?? {};
      write(env, {
        index: host,
        source: "server",
        kind: "event",
        cls: v.cls,
        edge_cls: v.cls,
        agent: v.agent,
        operator: v.operator,
        verification: v.verification,
        client: packClient(parseClient(ua, 0, !(v.cls === "human" || v.cls === "agentic_browser"))),
        path: opts.redactPath ? opts.redactPath(url.pathname) : url.pathname,
        utm_medium: url.searchParams.get("utm_medium") ?? "",
        referrer_host: external ? refHost : "",
        ai_source: aiSource(external ? refHost : "", utm),
        utm_source: utm,
        ...geoOf(cf),
        asn_org: String(cf.asOrganization ?? ""),
        ua,
        signals: v.signals.join(","),
        event_name: name.slice(0, 64),
        visitor: await visitorId(secret, host, ip, ua),
        detail: props ? JSON.stringify(props).slice(0, 1024) : "",
        confidence: v.confidence,
        asn: Number(cf.asn ?? 0),
        value,
        cf_bot_score: -1,
        bytes: -1,
      });
    })().catch(logErr("record_server_event_failed")),
  );
}
