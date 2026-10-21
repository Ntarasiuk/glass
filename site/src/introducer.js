// Introduces a browser to a Glass host for remote access over WebRTC. One Durable Object
// per host. The host holds a WebSocket here (proving it owns the key its id derives from);
// browsers send an offer, the host answers. Nothing else passes through: the media and
// input then flow directly between the two, end-to-end encrypted. The host signs its
// answers, so even this service can't impersonate it.

import { DurableObject } from 'cloudflare:workers';

const MAX_CLIENTS = 32;
const MAX_OFFERS_PER_SOCKET = 5;

export class Introducer extends DurableObject {
  async fetch(request) {
    if (request.headers.get('Upgrade') !== 'websocket') return new Response('Expected a WebSocket', { status: 426 });
    const url = new URL(request.url);
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

  /** The authenticated host socket, if any. */
  host() {
    return this.ctx.getWebSockets('host').find((ws) => ws.deserializeAttachment()?.authed);
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
      ws.serializeAttachment({ ...meta, authed: true });
      ws.send(JSON.stringify({ t: 'ready' }));
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
  try {
    const keyBytes = fromB64url(keyB64);
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', keyBytes));
    if (toB64url(digest.slice(0, 16)) !== hostId) return false;
    const key = await crypto.subtle.importKey('raw', keyBytes, { name: 'ECDSA', namedCurve: 'P-256' }, false, ['verify']);
    const data = new TextEncoder().encode(`glass-signal-v1\n${nonce}`);
    return await crypto.subtle.verify({ name: 'ECDSA', hash: 'SHA-256' }, key, fromB64url(sigB64), data);
  } catch {
    return false;
  }
}

/** Short-lived TURN credentials (Cloudflare Realtime), when configured. */
async function turnServers(env) {
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
