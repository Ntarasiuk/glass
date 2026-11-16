import { RANGE_LISTS, type RangeKey } from "./agents.ts";
import { inNets, parseCidr, type Net } from "./ip.ts";

const TTL_MS = 6 * 3600_000;
const FAIL_TTL_MS = 10 * 60_000;
// Isolate-level cache of immutable public data (not request state).
const cache = new Map<RangeKey, { exp: number; nets: Promise<Net[] | null> }>();

async function load(key: RangeKey): Promise<Net[] | null> {
  try {
    const res = await fetch(RANGE_LISTS[key], {
      cf: { cacheTtl: TTL_MS / 1000, cacheEverything: true },
      signal: AbortSignal.timeout(3000),
    });
    if (!res.ok) return null;
    const json = (await res.json()) as { prefixes?: { ipv4Prefix?: string; ipv6Prefix?: string }[] };
    const nets = (json.prefixes ?? [])
      .map((p) => parseCidr(p.ipv4Prefix ?? p.ipv6Prefix ?? ""))
      .filter((n): n is Net => n !== null);
    return nets.length ? nets : null;
  } catch {
    return null;
  }
}

function nets(key: RangeKey): Promise<Net[] | null> {
  const now = Date.now();
  const hit = cache.get(key);
  if (hit && hit.exp > now) return hit.nets;
  const p = load(key);
  cache.set(key, { exp: now + TTL_MS, nets: p });
  p.then((n) => {
    if (!n) cache.set(key, { exp: Date.now() + FAIL_TTL_MS, nets: p });
  });
  return p;
}

/** true = inside published ranges, false = outside (spoofed), null = couldn't check. */
export async function ipInRanges(key: RangeKey, ip: string): Promise<boolean | null> {
  const list = await nets(key);
  return list ? inNets(ip, list) : null;
}
