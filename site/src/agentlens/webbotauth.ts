/**
 * Web Bot Auth verification (RFC 9421 HTTP Message Signatures + draft-meunier-web-bot-auth).
 * Supports Ed25519 and the derived components bots actually sign.
 */

export interface ParsedSignature {
  label: string;
  components: string[]; // raw component identifiers, e.g. `"@authority";req`
  paramsRaw: string; // everything after `label=` (the @signature-params value)
  params: Record<string, string | number>;
  signature: Uint8Array<ArrayBuffer>;
}

export type WbaResult =
  | { ok: true; agentUrl: string; keyid: string }
  | { ok: false; reason: string; agentUrl?: string };

export type KeyFetcher = (directoryUrl: string) => Promise<JsonWebKey[] | null>;

/** Split a structured-field list/dict on top-level commas (not inside quotes/parens). */
function splitTop(s: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let q = false;
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === '"' && s[i - 1] !== "\\") q = !q;
    else if (!q && c === "(") depth++;
    else if (!q && c === ")") depth--;
    else if (!q && depth === 0 && c === ",") {
      out.push(s.slice(start, i).trim());
      start = i + 1;
    }
  }
  out.push(s.slice(start).trim());
  return out.filter(Boolean);
}

export function parseSignatureHeaders(sigInput: string, sig: string): ParsedSignature[] {
  const sigs = new Map<string, Uint8Array<ArrayBuffer>>();
  for (const m of splitTop(sig)) {
    const mm = /^([a-z0-9_*.-]+)=:([A-Za-z0-9+/=]+):$/i.exec(m);
    if (mm) sigs.set(mm[1]!, b64(mm[2]!));
  }
  const out: ParsedSignature[] = [];
  for (const member of splitTop(sigInput.replace(/\r?\n\s*/g, ""))) {
    const eq = member.indexOf("=");
    if (eq < 0) continue;
    const label = member.slice(0, eq).trim();
    const paramsRaw = member.slice(eq + 1).trim();
    const close = paramsRaw.indexOf(")");
    if (!paramsRaw.startsWith("(") || close < 0) continue;
    const inner = paramsRaw.slice(1, close);
    const components = inner.match(/"[^"]*"(?:;[a-z0-9_*.-]+(?:=[^\s;"]+|="[^"]*")?)*/gi) ?? [];
    const params: Record<string, string | number> = {};
    for (const p of paramsRaw.slice(close + 1).split(";")) {
      const [k, val] = p.split(/=(.*)/s, 2).map((x) => x?.trim() ?? "");
      if (!k) continue;
      params[k] = val?.startsWith('"') ? val.slice(1, -1) : Number(val);
    }
    const signature = sigs.get(label);
    if (signature) out.push({ label, components, paramsRaw, params, signature });
  }
  return out;
}

function componentValue(id: string, req: Request): string | null {
  const name = /^"([^"]*)"/.exec(id)?.[1]?.toLowerCase();
  if (!name) return null;
  const url = new URL(req.url);
  switch (name) {
    case "@authority":
      return url.host.toLowerCase();
    case "@method":
      return req.method.toUpperCase();
    case "@path":
      return url.pathname;
    case "@query":
      return url.search || "?";
    case "@target-uri":
      return url.href;
    case "@scheme":
      return url.protocol.replace(":", "");
    case "@request-target":
      return url.pathname + url.search;
    default: {
      const h = req.headers.get(name);
      return h === null ? null : h.trim();
    }
  }
}

export function signatureBase(p: ParsedSignature, req: Request): string | null {
  const lines: string[] = [];
  for (const id of p.components) {
    const val = componentValue(id, req);
    if (val === null) return null;
    lines.push(`${id}: ${val}`);
  }
  lines.push(`"@signature-params": ${p.paramsRaw}`);
  return lines.join("\n");
}

/** Signature-Agent: `"https://x"` (draft-03) or dict form `sig1="https://x"` (later drafts). */
export function parseSignatureAgent(header: string, label: string): string | null {
  const members = splitTop(header);
  for (const m of members) {
    const dict = /^([a-z0-9_*.-]+)="([^"]+)"$/i.exec(m);
    if (dict && dict[1] === label) return dict[2]!;
  }
  const plain = /^"([^"]+)"$/.exec(members[0] ?? "") ?? /^([a-z0-9_*.-]+)="([^"]+)"$/i.exec(members[0] ?? "");
  return plain ? plain[plain.length - 1]! : null;
}

export function directoryUrl(agentUrl: string): string | null {
  try {
    const u = new URL(agentUrl);
    if (u.protocol !== "https:") return null;
    if (u.pathname === "/" || u.pathname === "") u.pathname = "/.well-known/http-message-signatures-directory";
    return u.href;
  } catch {
    return null;
  }
}

/** RFC 7638 / 8037 JWK thumbprint for OKP keys. */
export async function jwkThumbprint(jwk: JsonWebKey): Promise<string> {
  const canonical = `{"crv":"${jwk.crv}","kty":"${jwk.kty}","x":"${jwk.x}"}`;
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical));
  return b64url(new Uint8Array(digest));
}

export async function verifyWebBotAuth(req: Request, fetchKeys: KeyFetcher, nowSec = Date.now() / 1000): Promise<WbaResult> {
  const input = req.headers.get("signature-input");
  const sig = req.headers.get("signature");
  if (!input || !sig) return { ok: false, reason: "missing_headers" };

  const parsed = parseSignatureHeaders(input, sig).find((p) => p.params.tag === "web-bot-auth");
  if (!parsed) return { ok: false, reason: "no_web_bot_auth_tag" };

  const agentHeader = req.headers.get("signature-agent");
  const agentUrl = agentHeader ? parseSignatureAgent(agentHeader, parsed.label) : null;
  if (!agentUrl) return { ok: false, reason: "missing_signature_agent" };
  if (!parsed.components.some((c) => c.startsWith('"signature-agent"')))
    return { ok: false, reason: "signature_agent_not_signed", agentUrl };

  const created = Number(parsed.params.created);
  const expires = Number(parsed.params.expires);
  if (!Number.isFinite(created) || created > nowSec + 60) return { ok: false, reason: "bad_created", agentUrl };
  if (!Number.isFinite(expires) || expires < nowSec) return { ok: false, reason: "expired", agentUrl };
  if (parsed.params.alg !== undefined && parsed.params.alg !== "ed25519") return { ok: false, reason: "unsupported_alg", agentUrl };

  const dir = directoryUrl(agentUrl);
  if (!dir) return { ok: false, reason: "bad_agent_url", agentUrl };
  const base = signatureBase(parsed, req);
  if (base === null) return { ok: false, reason: "missing_component", agentUrl };

  const keys = await fetchKeys(dir);
  if (!keys) return { ok: false, reason: "directory_unavailable", agentUrl };

  const keyid = String(parsed.params.keyid ?? "");
  for (const jwk of keys) {
    if (jwk.kty !== "OKP" || jwk.crv !== "Ed25519" || !jwk.x) continue;
    if ((await jwkThumbprint(jwk)) !== keyid) continue;
    const key = await crypto.subtle.importKey("jwk", { kty: "OKP", crv: "Ed25519", x: jwk.x }, { name: "Ed25519" }, false, ["verify"]);
    const ok = await crypto.subtle.verify({ name: "Ed25519" }, key, parsed.signature, new TextEncoder().encode(base));
    return ok ? { ok: true, agentUrl, keyid } : { ok: false, reason: "bad_signature", agentUrl };
  }
  return { ok: false, reason: "unknown_key", agentUrl };
}

function b64(s: string): Uint8Array<ArrayBuffer> {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
