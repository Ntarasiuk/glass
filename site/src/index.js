// getglass.lol: landing page, one-line installer, release downloads (counted in D1),
// and the feedback API. Static files come from ./public via the ASSETS binding.

const FEEDBACK_MAX = 4000;
const FEEDBACK_PER_HOUR = 5;
// Command-line fetchers get the install script at the bare domain (`curl getglass.lol | sh`).
const CLI_AGENT = /^(curl|wget|fetch|httpie|python-requests|go-http-client)\b/i;

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const ua = request.headers.get('user-agent') ?? '';

    if (url.pathname === '/api/feedback') {
      if (request.method === 'OPTIONS') return cors(new Response(null, { status: 204 }));
      if (request.method === 'POST') return cors(await feedback(request, env));
      return cors(json({ error: 'method not allowed' }, 405));
    }

    if (url.pathname === '/api/stats') return stats(request, env);

    const wantsScript = url.pathname === '/install.sh' || (url.pathname === '/' && CLI_AGENT.test(ua));
    if (wantsScript) {
      ctx.waitUntil(recordDownload(env, request, 'script', 'install.sh'));
      const res = await env.ASSETS.fetch(new URL('/install.sh', url));
      return new Response(res.body, {
        headers: { 'content-type': 'text/x-shellscript; charset=utf-8', 'cache-control': 'no-store' },
      });
    }

    if (url.pathname.startsWith('/releases/') && url.pathname.endsWith('.tar.gz')) {
      ctx.waitUntil(recordDownload(env, request, 'release', url.pathname.slice('/releases/'.length)));
    }
    return env.ASSETS.fetch(request);
  },
};

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
