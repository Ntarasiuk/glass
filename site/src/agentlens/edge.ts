/**
 * Edge tests and surveys on the hot path: what a site runs (read from the dashboard, cached in memory) and how a
 * variant is applied to a page. Never waits on the network and never throws into the request: with no cached
 * config, or a bad selector, the page goes out untouched.
 */
import { BLOCKED_TAGS, cleanEdgeTests, type EdgeTest, type VariantDef } from "./variants.ts";
import { cleanSurveys, type Survey } from "./surveys.ts";
import { logErr, type AgentLensEnv } from "./shared.ts";

const TTL_MS = 60_000;
const cache = new Map<string, { at: number; tests: EdgeTest[]; surveys: Survey[] }>();
const NONE = { tests: [] as EdgeTest[], surveys: [] as Survey[] };

/** Tests and surveys for a host, from memory. A stale or missing entry is refreshed in the background for later requests. */
export function configFor(env: AgentLensEnv, host: string, ctx: ExecutionContext, now = Date.now()): { tests: EdgeTest[]; surveys: Survey[] } {
  if (!env.AL_DASHBOARD) return NONE;
  const c = cache.get(host);
  if (!c || now - c.at > TTL_MS) {
    // Stamp first, so concurrent requests don't all refetch.
    cache.set(host, { at: now, tests: c?.tests ?? [], surveys: c?.surveys ?? [] });
    ctx.waitUntil(refresh(env.AL_DASHBOARD, host).catch(logErr("variants_refresh_failed")));
  }
  return c ?? NONE;
}

export const testsFor = (env: AgentLensEnv, host: string, ctx: ExecutionContext, now = Date.now()): EdgeTest[] => configFor(env, host, ctx, now).tests;

export async function refresh(dashboard: string, host: string, f: typeof fetch = fetch): Promise<void> {
  const u = new URL("/api/public/variants", dashboard);
  u.searchParams.set("host", host);
  const res = await f(u, { signal: AbortSignal.timeout(3000) });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const body = (await res.json()) as { tests?: unknown; surveys?: unknown };
  cache.set(host, { at: Date.now(), tests: cleanEdgeTests(body.tests), surveys: cleanSurveys(body.surveys) });
}

/** For tests. */
export const resetVariantCache = () => cache.clear();

/** Register a variant's changes on a rewriter. Text is escaped by HTMLRewriter; blocked elements are skipped. */
export function applyVariant(rw: HTMLRewriter, variant: VariantDef): HTMLRewriter {
  for (const op of variant.ops) {
    try {
      rw.on(op.selector, {
        element(el) {
          if (BLOCKED_TAGS.has(el.tagName.toLowerCase())) return;
          if (op.action === "text") el.setInnerContent(op.value);
          else if (op.attr) el.setAttribute(op.attr, op.value);
        },
      });
    } catch (err) {
      // A selector HTMLRewriter can't parse: skip this change, keep the page.
      console.error(JSON.stringify({ msg: "variant_selector_rejected", selector: op.selector, err: String(err) }));
    }
  }
  return rw;
}
