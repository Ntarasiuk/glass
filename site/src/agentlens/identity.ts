import { SIGNED_AGENTS } from "./agents.ts";
import { hostOf } from "./referrer.ts";
import type { Verdict } from "./taxonomy.ts";
import { verifyWebBotAuth, type KeyFetcher } from "./webbotauth.ts";
import { ipInRanges } from "./ranges.ts";

/** Resolve claimed identities: Web Bot Auth first (cryptographic), then published IP ranges. */
export async function verifyIdentity(req: Request, v: Verdict, ip: string): Promise<Verdict> {
  if (req.headers.has("signature-input")) {
    const r = await verifyWebBotAuth(req, fetchKeys);
    if (r.ok) {
      const host = hostOf(r.agentUrl);
      const known = SIGNED_AGENTS[host];
      return {
        ...v,
        cls: known?.cls ?? v.cls,
        agent: known?.name ?? (v.agent && v.agent !== "Signed agent" ? v.agent : host),
        operator: known?.operator ?? (v.operator || host),
        verification: "signed",
        confidence: 0.99,
        signals: [...v.signals, "wba_valid"],
      };
    }
    // Present but invalid signature is a strong spoofing signal; unknown key / unreachable directory is not.
    const spoof = r.reason === "bad_signature" || r.reason === "expired";
    v = { ...v, verification: spoof ? "spoofed" : v.verification, signals: [...v.signals, `wba:${r.reason}`] };
    if (spoof) return v;
  }
  if (v.rangeKey && ip) {
    const inside = await ipInRanges(v.rangeKey, ip);
    if (inside === true) return { ...v, verification: "ip_verified", confidence: 0.98 };
    if (inside === false) return { ...v, verification: "spoofed", confidence: 0.9, signals: [...v.signals, "ip_not_in_ranges"] };
  }
  return v;
}

// Isolate-level cache of public key directories (immutable public data, bounded).
const keyCache = new Map<string, { exp: number; keys: Promise<JsonWebKey[] | null> }>();

const fetchKeys: KeyFetcher = (dirUrl) => {
  const hit = keyCache.get(dirUrl);
  if (hit && hit.exp > Date.now()) return hit.keys;
  const keys = (async () => {
    try {
      const res = await fetch(dirUrl, {
        headers: { accept: "application/http-message-signatures-directory+json, application/json" },
        cf: { cacheTtl: 3600, cacheEverything: true },
        signal: AbortSignal.timeout(2500),
      });
      if (!res.ok || Number(res.headers.get("content-length") ?? 0) > 64 * 1024) return null;
      const json = (await res.json()) as { keys?: JsonWebKey[] };
      return Array.isArray(json.keys) ? json.keys.slice(0, 32) : null;
    } catch {
      return null;
    }
  })();
  if (keyCache.size > 500) keyCache.clear(); // bound memory against header-driven fan-out
  keyCache.set(dirUrl, { exp: Date.now() + 10 * 60_000, keys });
  return keys;
};
