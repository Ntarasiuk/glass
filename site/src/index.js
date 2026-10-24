// glassdesk.dev: landing page, one-line installer, release downloads (counted in D1),
// the feedback API, and the introducer for remote access (WebRTC signaling, one Durable
// Object per host). Static files come from ./public via the ASSETS binding; the web client
// for remote access is served from /app/.

export { Introducer } from './introducer.js';

const FEEDBACK_MAX = 4000;
const FEEDBACK_PER_HOUR = 5;
// Command-line fetchers get the install script at the bare domain (`curl glassdesk.dev | sh`).
const CLI_AGENT = /^(curl|wget|fetch|httpie|python-requests|go-http-client)\b/i;

// Agent discovery on the home page: Link relations, and a markdown twin for Accept: text/markdown.
const HOME_LINKS = [
  '<https://glassdesk.dev/>; rel="canonical"',
  '</index.md>; rel="alternate"; type="text/markdown"',
  '</sitemap.xml>; rel="sitemap"; type="application/xml"',
  '</llms.txt>; rel="describedby"; type="text/plain"',
  '</docs>; rel="help"',
].join(', ');
const HSTS = 'max-age=31536000; includeSubDomains';

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const ua = request.headers.get('user-agent') ?? '';

    // HTTPS only (`curl -fsSL glassdesk.dev` follows the redirect).
    // CF-Visitor carries the client's scheme at the edge (local dev has none).
    if (/"scheme":"http"/.test(request.headers.get('cf-visitor') ?? '')) {
      url.protocol = 'https:';
      return Response.redirect(url.toString(), 301);
    }

    if (url.pathname === '/api/feedback') {
      if (request.method === 'OPTIONS') return cors(new Response(null, { status: 204 }));
      if (request.method === 'POST') return cors(await feedback(request, env));
      return cors(json({ error: 'method not allowed' }, 405));
    }

    if (url.pathname === '/api/stats') return stats(request, env);

    // /signal/host/<id> (the host) and /signal/connect/<id> (a browser).
    const signal = url.pathname.match(/^\/signal\/(host|connect)\/([A-Za-z0-9_-]{22})$/);
    if (signal) {
      const [, kind, id] = signal;
      const target = new URL(`https://introducer/?role=${kind === 'host' ? 'host' : 'client'}&id=${id}`);
      const headers = new Headers(request.headers);
      headers.set('X-Client-IP', request.headers.get('CF-Connecting-IP') ?? '');
      return env.INTRODUCER.get(env.INTRODUCER.idFromName(id)).fetch(new Request(target, { headers }));
    }

    // An agent asking for markdown gets the page's markdown twin, even from curl; `curl … | sh` never asks.
    const wantsMarkdown = /\btext\/markdown\b/i.test(request.headers.get('accept') ?? '');
    const wantsScript = url.pathname === '/install.sh' || (url.pathname === '/' && CLI_AGENT.test(ua) && !wantsMarkdown);
    if (wantsScript) {
      ctx.waitUntil(recordDownload(env, request, 'script', 'install.sh'));
      const res = await env.ASSETS.fetch(new URL('/install.sh', url));
      return new Response(res.body, {
        headers: { 'content-type': 'text/x-shellscript; charset=utf-8', 'cache-control': 'no-store', 'strict-transport-security': HSTS },
      });
    }

    // The web client for remote access lives at /app/, with the slash, so its relative URLs
    // resolve under /app/. drop-trailing-slash would redirect it to /app and break them.
    if (url.pathname === '/app') {
      url.pathname = '/app/';
      return Response.redirect(url.toString(), 301);
    }
    if (url.pathname === '/app/') return env.ASSETS.fetch(new Request(new URL('/app', url), request));

    if (url.pathname === '/' && (request.method === 'GET' || request.method === 'HEAD')) {
      const res = await env.ASSETS.fetch(wantsMarkdown ? new Request(new URL('/index.md', url), request) : request);
      const out = new Response(res.body, res);
      if (wantsMarkdown) out.headers.set('content-type', 'text/markdown; charset=utf-8');
      out.headers.append('vary', 'Accept');
      out.headers.set('link', HOME_LINKS);
      out.headers.set('strict-transport-security', HSTS);
      return out;
    }

    if (url.pathname.endsWith('.mp4')) return ranged(await env.ASSETS.fetch(request), request);

    if (url.pathname.startsWith('/releases/') && url.pathname.endsWith('.tar.gz')) {
      ctx.waitUntil(recordDownload(env, request, 'release', url.pathname.slice('/releases/'.length)));
    }
    return env.ASSETS.fetch(request);
  },
};

/** Answers `Range: bytes=…` from a whole (small) asset: static assets ignore Range. */
export async function ranged(res, request) {
  const range = request.headers.get('range');
  const headers = new Headers(res.headers);
  headers.set('accept-ranges', 'bytes');
  if (!res.ok || !range) return new Response(res.body, { status: res.status, headers });
  const body = new Uint8Array(await res.arrayBuffer());
  const size = body.length;
  const m = /^bytes=(\d*)-(\d*)$/.exec(range.trim());
  let start = m && m[1] !== '' ? Number(m[1]) : NaN;
  let end = m && m[2] !== '' ? Number(m[2]) : size - 1;
  if (m && m[1] === '' && m[2] !== '') {
    start = Math.max(0, size - Number(m[2])); // the last N bytes
    end = size - 1;
  }
  if (!m || !(start >= 0) || start >= size || end < start) {
    headers.set('content-range', `bytes */${size}`);
    return new Response(null, { status: 416, headers });
  }
  end = Math.min(end, size - 1);
  headers.set('content-range', `bytes ${start}-${end}/${size}`);
  headers.set('content-length', String(end - start + 1));
  return new Response(request.method === 'HEAD' ? null : body.slice(start, end + 1), { status: 206, headers });
}

async function feedback(request, env) {
  let body;
  try {
    body = await request.json();
  } catch {
    return json({ error: 'expected JSON' }, 400);
  }
  const message = String(body.message ?? '').trim();
  if (!message) return json({ error: 'message is required' }, 400);
  if (message.length > FEEDBACK_MAX) return json({ error: `message is over ${FEEDBACK_MAX} characters` }, 400);
  const email = clip(body.email, 200);
  if (email && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) return json({ error: 'that email looks off' }, 400);

  const visitor = await visitorId(request, env);
  const hourAgo = Date.now() - 3600_000;
  const recent = await env.DB.prepare('SELECT COUNT(*) AS n FROM feedback WHERE visitor = ? AND ts > ?')
    .bind(visitor, hourAgo)
    .first('n');
  if (recent >= FEEDBACK_PER_HOUR) return json({ error: 'thanks! that is plenty for now, try again later' }, 429);

  await env.DB.prepare(
    'INSERT INTO feedback (ts, message, email, source, version, os, country, visitor) VALUES (?, ?, ?, ?, ?, ?, ?, ?)',
  )
    .bind(
      Date.now(),
      message,
      email,
      clip(body.source, 20) ?? 'web',
      clip(body.version, 40),
      clip(body.os, 80),
      request.cf?.country ?? null,
      visitor,
    )
    .run();
  return json({ ok: true });
}

async function recordDownload(env, request, kind, file) {
  try {
    await env.DB.prepare('INSERT INTO downloads (ts, kind, file, country, user_agent, visitor) VALUES (?, ?, ?, ?, ?, ?)')
      .bind(
        Date.now(),
        kind,
        file,
        request.cf?.country ?? null,
        clip(request.headers.get('user-agent'), 200),
        await visitorId(request, env),
      )
      .run();
  } catch (e) {
    console.error('recordDownload failed', e);
  }
}

/** Totals for the last 30 days. Requires `Authorization: Bearer <STATS_KEY>`. */
async function stats(request, env) {
  const auth = request.headers.get('authorization') ?? '';
  if (!env.STATS_KEY || auth !== `Bearer ${env.STATS_KEY}`) return json({ error: 'unauthorized' }, 401);
  const since = Date.now() - 30 * 86400_000;
  const [downloads, feedbackRows] = await Promise.all([
    env.DB.prepare(
      'SELECT kind, COUNT(*) AS total, COUNT(DISTINCT visitor) AS unique_visitors FROM downloads WHERE ts > ? GROUP BY kind',
    )
      .bind(since)
      .all(),
    env.DB.prepare('SELECT ts, source, version, message, email FROM feedback ORDER BY ts DESC LIMIT 50').all(),
  ]);
  return json({ downloads: downloads.results, feedback: feedbackRows.results });
}

/** Salted SHA-256 of the client IP: lets us count unique visitors without storing IPs. */
async function visitorId(request, env) {
  const ip = request.headers.get('cf-connecting-ip') ?? '';
  const data = new TextEncoder().encode(`${env.VISITOR_SALT ?? ''}:${ip}`);
  const digest = await crypto.subtle.digest('SHA-256', data);
  return [...new Uint8Array(digest).slice(0, 12)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

function clip(value, max) {
  if (value === undefined || value === null) return null;
  const s = String(value).trim();
  return s ? s.slice(0, max) : null;
}

function json(data, status = 200) {
  return new Response(JSON.stringify(data), { status, headers: { 'content-type': 'application/json' } });
}

// The in-app feedback button posts from the user's own Glass host origin.
function cors(response) {
  const res = new Response(response.body, response);
  res.headers.set('access-control-allow-origin', '*');
  res.headers.set('access-control-allow-methods', 'POST, OPTIONS');
  res.headers.set('access-control-allow-headers', 'content-type');
  return res;
}
