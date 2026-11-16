import { CLASSES, classAt, classIndex, type TrafficClass } from "./taxonomy.ts";
import { b64url } from "./webbotauth.ts";

const enc = new TextEncoder();
const keyCache = new Map<string, Promise<CryptoKey>>(); // immutable per secret, safe across requests

function hmacKey(secret: string): Promise<CryptoKey> {
  let k = keyCache.get(secret);
  if (!k) {
    k = crypto.subtle.importKey("raw", enc.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
    keyCache.set(secret, k);
  }
  return k;
}

async function mac(secret: string, msg: string): Promise<string> {
  const sig = await crypto.subtle.sign("HMAC", await hmacKey(secret), enc.encode(msg));
  return b64url(new Uint8Array(sig)).slice(0, 22);
}

/**
 * Page token injected next to the beacon: binds a beacon hit to the edge verdict + host,
 * so beacons can't be forged for other sites or claim a different edge class.
 */
export async function signToken(secret: string, host: string, cls: TrafficClass, now = Date.now()): Promise<string> {
  const ts = Math.floor(now / 1000).toString(36);
  const body = `${ts}.${classIndex(cls)}`;
  return `${body}.${await mac(secret, `${body}.${host}`)}`;
}

export const TOKEN_TTL_SEC = 24 * 3600;

export async function verifyToken(secret: string, host: string, token: string, now = Date.now()): Promise<TrafficClass | null> {
  const parts = token.split(".");
  if (parts.length !== 3) return null;
  const [ts, ci, sig] = parts as [string, string, string];
  const age = now / 1000 - parseInt(ts, 36);
  if (!(age >= -60 && age <= TOKEN_TTL_SEC)) return null;
  const cls = classAt(Number(ci));
  if (!cls || !/^\d+$/.test(ci) || Number(ci) >= CLASSES.length) return null;
  const expected = await mac(secret, `${ts}.${ci}.${host}`);
  if (!timingSafeEqualStr(expected, sig)) return null;
  return cls;
}

function timingSafeEqualStr(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

const hex = (b: ArrayBuffer, n: number) => [...new Uint8Array(b).slice(0, n)].map((x) => x.toString(16).padStart(2, "0")).join("");

/** IPv6 counts by its /64 (one home or office): devices rotate the rest of the address. */
export function network(ip: string): string {
  if (!ip.includes(":")) return ip;
  const [head = "", tail] = ip.split("::");
  const h = head ? head.split(":") : [];
  const t = tail ? tail.split(":") : [];
  const groups = tail === undefined ? h : [...h, ...Array<string>(Math.max(0, 8 - h.length - t.length)).fill("0"), ...t];
  return groups.slice(0, 4).map((g) => g.toLowerCase().padStart(4, "0")).join(":") + "::/64";
}

/**
 * Cookieless visitor id (Plausible-style): rotates daily, unlinkable across days and sites, not reversible without
 * the secret. Shaped "<visitor>.<network>": the visitor part is one browser or client (IP + user agent) on one day; the network part
 * is everyone on that IP (or IPv6 /64) that day, so a page view in a browser can be linked to a `curl … | sh`
 * install from the same place. Both rotate daily and can't be reversed without the secret.
 */
export async function visitorId(secret: string, host: string, ip: string, ua: string, now = Date.now()): Promise<string> {
  const day = new Date(now).toISOString().slice(0, 10);
  const [v, net] = await Promise.all([
    crypto.subtle.digest("SHA-256", enc.encode(`${day}|${secret}|${host}|${ip}|${ua}`)),
    crypto.subtle.digest("SHA-256", enc.encode(`${day}|${secret}|${host}|net|${network(ip)}`)),
  ]);
  return `${hex(v, 8)}.${hex(net, 4)}`;
}
