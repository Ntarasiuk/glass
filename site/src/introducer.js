// Introduces a browser to a Glass host for remote access over WebRTC. One Durable Object
// per host. The host holds a WebSocket here (proving it owns the key its id derives from);
// browsers send an offer, the host answers. Nothing else passes through: the media and
// input then flow directly between the two, end-to-end encrypted. The host signs its
// answers, so even this service can't impersonate it.

import { DurableObject } from 'cloudflare:workers';

const MAX_CLIENTS = 32;
const MAX_OFFERS_PER_SOCKET = 5;
// How long a command waits for its computer (a script may take a minute).
const RELAY_WAIT_MS = 70_000;
const RELAY_IN_FLIGHT = 4;

export class Introducer extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    // The host's heartbeat, answered without waking this object.
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair('ping', 'pong'));
    // Commands waiting for the host's answer (id → resolve); lives only while a request is open.
    this.pending = new Map();
  }

  async fetch(request) {
    const url = new URL(request.url);
    // For glassdesk.dev/computers. Not secret: a browser that connects is told the same.
    if (url.searchParams.get('role') === 'status') return Response.json({ online: Boolean(this.host()) });
    if (url.searchParams.get('role') === 'relay' && request.method === 'POST') return this.relay(request);
    if (request.headers.get('Upgrade') !== 'websocket') return new Response('Expected a WebSocket', { status: 426 });
    const role = url.searchParams.get('role');
    const hostId = url.searchParams.get('id');
    const { 0: client, 1: server } = new WebSocketPair();
    if (role === 'host') {
      const nonce = randomId(24);
      this.ctx.acceptWebSocket(server, ['host']);
      server.serializeAttachment({ role: 'host', hostId, nonce, authed: false });
      server.send(JSON.stringify({ t: 'challenge', nonce }));
    } else {
      if (this.ctx.getWebSockets('client').length >= MAX_CLIENTS) return new Response('Busy', { status: 503 });
      const session = randomId(12);
      const iceServers = await turnServers(this.env).catch(() => []);
      this.ctx.acceptWebSocket(server, ['client', `session:${session}`]);
      server.serializeAttachment({ role: 'client', session, ip: request.headers.get('X-Client-IP'), iceServers, offers: 0 });
      server.send(JSON.stringify({ t: 'hello', online: Boolean(this.host()), iceServers }));
    }
    return new Response(null, { status: 101, webSocket: client });
  }

  /**
   * Hands a signed command or an enrollment (from /api/account) to the host and returns its
   * answer. This object only carries them: the host checks the passkey's signature itself.
   */
  async relay(request) {
    const host = this.host();
    if (!host) return Response.json({ ok: false, message: 'This computer is offline, or remote access is turned off on it.' }, { status: 409 });
    let body;
    try {
      body = await request.json();
    } catch {
      return Response.json({ ok: false, message: 'bad request' }, { status: 400 });
    }
    const { kind, ...payload } = body ?? {};
    if (kind !== 'command' && kind !== 'enroll') return Response.json({ ok: false, message: 'bad request' }, { status: 400 });
    // A few at a time: a flood of commands never reaches the computer.
    if (this.pending.size >= RELAY_IN_FLIGHT) return Response.json({ ok: false, message: 'This computer is busy with other actions. Try again in a moment.' }, { status: 429 });
    const id = randomId(12);
    const answer = new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        resolve({ ok: false, message: "The computer didn't answer in time." });
      }, RELAY_WAIT_MS);
      this.pending.set(id, (r) => {
        clearTimeout(timer);
        this.pending.delete(id);
        resolve(r);
      });
    });
    host.send(JSON.stringify({ t: kind, id, ...payload }));
    return Response.json(await answer);
  }

  /**
   * The host's newest signed-in connection. A host that reconnected can leave an older
   * socket behind that looks open but goes nowhere; offers must never be sent to it.
   */
  host() {
    let best;
    for (const ws of this.ctx.getWebSockets('host')) {
      const meta = ws.deserializeAttachment();
      if (meta?.authed && (!best || (meta.authedAt ?? 0) >= (best.meta.authedAt ?? 0))) best = { ws, meta };
    }
    return best?.ws;
  }

  async webSocketMessage(ws, raw) {
    let msg;
    try {
      msg = JSON.parse(typeof raw === 'string' ? raw : new TextDecoder().decode(raw));
    } catch {
      return;
    }
    const meta = ws.deserializeAttachment() ?? {};
    if (meta.role === 'host') return this.fromHost(ws, meta, msg);
    if (meta.role === 'client') return this.fromClient(ws, meta, msg);
  }

  async fromHost(ws, meta, msg) {
    if (!meta.authed) {
      if (msg.t !== 'auth' || !(await verifyHost(meta.hostId, meta.nonce, msg.key, msg.sig))) {
        ws.close(4001, 'unauthorized');
        return;
      }
      // One live host per id: a newer connection replaces an older one.
      for (const other of this.ctx.getWebSockets('host')) if (other !== ws) other.close(4000, 'replaced');
      ws.serializeAttachment({ ...meta, authed: true, authedAt: Date.now() });
      ws.send(JSON.stringify({ t: 'ready' }));
      return;
    }
    if (msg.t === 'result' && typeof msg.id === 'string') {
      this.pending.get(msg.id)?.({ ok: msg.ok === true, message: String(msg.message ?? '').slice(0, 2500) });
      return;
    }
    if ((msg.t === 'answer' || msg.t === 'error') && typeof msg.session === 'string') {
      const [client] = this.ctx.getWebSockets(`session:${msg.session}`);
      if (!client) return;
      const { t, sdp, sig, msg: text } = msg;
      client.send(JSON.stringify(t === 'answer' ? { t, sdp, sig } : { t, msg: text }));
    }
  }

  async fromClient(ws, meta, msg) {
    if (msg.t !== 'offer' || typeof msg.sdp !== 'string' || msg.sdp.length > 20_000) return;
    if (meta.offers >= MAX_OFFERS_PER_SOCKET) {
      ws.close(4029, 'too many offers');
      return;
    }
    ws.serializeAttachment({ ...meta, offers: meta.offers + 1 });
    const host = this.host();
    if (!host) {
      ws.send(JSON.stringify({ t: 'error', msg: "This computer is offline, or remote access is turned off on it." }));
      return;
    }
    host.send(JSON.stringify({ t: 'offer', session: meta.session, sdp: msg.sdp, ip: meta.ip, iceServers: meta.iceServers }));
  }

  async webSocketClose(ws, code) {
    try { ws.close(code, 'closing'); } catch {}
  }
}

/** The host proves it holds the key whose fingerprint is its id. */
async function verifyHost(hostId, nonce, keyB64, sigB64) {
  return (await verifySigned(keyB64, `glass-signal-v1\n${nonce}`, sigB64)) === hostId;
}

/**
 * The id of the host whose key (`keyB64`, a raw P-256 point) signed `message`, or null.
 * The id is the key's fingerprint, so only the key's holder speaks for it.
 */
export async function verifySigned(keyB64, message, sigB64) {
  try {
    const keyBytes = fromB64url(keyB64);
    const key = await crypto.subtle.importKey('raw', keyBytes, { name: 'ECDSA', namedCurve: 'P-256' }, false, ['verify']);
    const ok = await crypto.subtle.verify({ name: 'ECDSA', hash: 'SHA-256' }, key, fromB64url(sigB64), new TextEncoder().encode(message));
    return ok ? await hostIdFor(keyBytes) : null;
  } catch {
    return null;
  }
}

/** The introducer's id for a host: the first 16 bytes of its key's SHA-256. */
export async function hostIdFor(keyBytes) {
  return toB64url(new Uint8Array(await crypto.subtle.digest('SHA-256', keyBytes)).slice(0, 16));
}

/** Which of these hosts are connected here now: { id: true | false }. */
export async function hostsOnline(env, ids) {
  const entries = await Promise.all(
    ids.map((id) =>
      env.INTRODUCER.get(env.INTRODUCER.idFromName(id))
        .fetch(`https://introducer/?role=status&id=${id}`)
        .then((r) => r.json())
        .then((r) => [id, Boolean(r.online)], () => [id, false]),
    ),
  );
  return Object.fromEntries(entries);
}

/** Short-lived TURN credentials: your own coturn (self-hosting), or Cloudflare Realtime. */
async function turnServers(env) {
  if (env.TURN_URLS && env.TURN_SECRET) return [await coturnCredentials(env.TURN_URLS, env.TURN_SECRET)];
  if (!env.TURN_KEY_ID || !env.TURN_KEY_API_TOKEN) return [];
  const res = await fetch(`https://rtc.live.cloudflare.com/v1/turn/keys/${env.TURN_KEY_ID}/credentials/generate-ice-servers`, {
    method: 'POST',
    headers: { authorization: `Bearer ${env.TURN_KEY_API_TOKEN}`, 'content-type': 'application/json' },
    body: JSON.stringify({ ttl: 86400 }),
  });
  if (!res.ok) return [];
  const { iceServers } = await res.json();
  // Only TURN entries: everyone already uses Cloudflare's public STUN.
  return (Array.isArray(iceServers) ? iceServers : [iceServers])
    .map((s) => ({ ...s, urls: [].concat(s.urls).filter((u) => u.startsWith('turn')) }))
    .filter((s) => s.urls.length);
}

/**
 * coturn's `use-auth-secret` scheme (TURN REST API): the username is an expiry time, the
 * password its HMAC-SHA1 under the shared secret. `urls`: comma-separated turn: URLs.
 */
export async function coturnCredentials(urls, secret, now = Date.now()) {
  const username = `${Math.floor(now / 1000) + 86400}:glass`;
  const key = await crypto.subtle.importKey('raw', new TextEncoder().encode(secret), { name: 'HMAC', hash: 'SHA-1' }, false, ['sign']);
  const mac = new Uint8Array(await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(username)));
  return {
    urls: urls.split(',').map((u) => u.trim()).filter((u) => u.startsWith('turn')),
    username,
    credential: btoa(String.fromCharCode(...mac)),
  };
}

function randomId(bytes) {
  return toB64url(crypto.getRandomValues(new Uint8Array(bytes)));
}

export function toB64url(bytes) {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function fromB64url(text) {
  const s = atob(text.replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(s, (c) => c.charCodeAt(0));
}
