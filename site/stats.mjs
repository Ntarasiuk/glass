#!/usr/bin/env node
// Who's downloading Glass, and what they say: installs and updates (D1), the site's traffic
// (Google Analytics), GitHub release downloads, and the latest feedback.
//
//   node stats.mjs [--days 30]
//   node stats.mjs --mark-me [label]   leave this machine's address out of the numbers
//
// Needs wrangler signed in (for D1) and, for Google Analytics, GLASS_GA_KEY: the path to a
// service account key with read access to the Glass property.

import { execFileSync } from 'node:child_process';
import { createSign } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const SITE = 'https://glassdesk.dev';
const GA_PROPERTY = 'properties/560592665';
const REPO = 'Ntarasiuk/glass';
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const days = Number(args[args.indexOf('--days') + 1]) || 30;

function d1(sql) {
  let out;
  try {
    out = execFileSync('npx', ['wrangler', 'd1', 'execute', 'getglass', '--remote', '--json', '--command', sql], {
      cwd: here,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
  } catch (e) {
    const err = JSON.parse(e.stdout || '{}').error;
    console.error(`D1: ${err?.notes?.[0]?.text ?? err?.text ?? e.message}`);
    if (/internal_visitors|no such column/.test(JSON.stringify(err))) console.error('Apply the migrations first: npx wrangler d1 migrations apply getglass --remote');
    process.exit(1);
  }
  return JSON.parse(out)[0].results;
}

if (args[0] === '--mark-me') {
  // Both of this machine's addresses: IPv4 and IPv6 reach the site as different visitors.
  const ids = ['-4', '-6'].flatMap((family) => {
    try {
      return [JSON.parse(execFileSync('curl', ['-fsS', '--max-time', '10', family, `${SITE}/api/visitor`], { encoding: 'utf8' })).visitor];
    } catch {
      return [];
    }
  });
  const visitor = ids.join(', ');
  const label = (args[1] ?? 'mine').replace(/'/g, "''");
  for (const id of ids.filter((v) => /^[0-9a-f]{24}$/.test(v))) {
    d1(`INSERT OR REPLACE INTO internal_visitors (visitor, label, added) VALUES ('${id}', '${label}', ${Date.now()})`);
  }
  console.log(`Left out of the numbers from now on (and in the past): this address (${visitor}, "${args[1] ?? 'mine'}").`);
  process.exit(0);
}

const since = Date.now() - days * 86400_000;
const notOurs = `ts > ${since} AND visitor NOT IN (SELECT visitor FROM internal_visitors)`;
const table = (rows) => (rows.length ? console.table(rows) : console.log('  (none)'));

console.log(`\nGlass, the last ${days} days (our own machines left out)\n`);

console.log('Downloads (glassdesk.dev)');
table(
  d1(`SELECT CASE WHEN file LIKE '%darwin%' THEN 'macOS' WHEN file LIKE '%linux%' THEN 'Linux' ELSE file END AS os,
        COALESCE(via, 'unknown') AS via, COUNT(*) AS downloads, COUNT(DISTINCT visitor) AS people
      FROM downloads WHERE kind = 'release' AND ${notOurs} GROUP BY os, via ORDER BY downloads DESC`),
);
console.log('  via: install = new, update = replacing a version, unknown = before the installer said (or a direct download)');

console.log('\nNew installs per day');
table(
  d1(`SELECT date(ts / 1000, 'unixepoch') AS day, COUNT(DISTINCT visitor) AS people
      FROM downloads WHERE kind = 'release' AND via = 'install' AND ${notOurs} GROUP BY day ORDER BY day`),
);

console.log('\nScripts fetched (include agents and bots reading the page with curl)');
table(
  d1(`SELECT file, COUNT(*) AS fetches, COUNT(DISTINCT visitor) AS people FROM downloads
      WHERE kind = 'script' AND ${notOurs} GROUP BY file`),
);

const feedback = d1(`SELECT datetime(ts / 1000, 'unixepoch') AS at, source, version, substr(message, 1, 100) AS message, email
  FROM feedback WHERE ts > ${since} ORDER BY ts DESC LIMIT 15`);
console.log(`\nFeedback (${feedback.length})`);
table(feedback);

console.log('\nGitHub release downloads (all time, including redirects from glassdesk.dev)');
try {
  const releases = JSON.parse(execFileSync('gh', ['api', `repos/${REPO}/releases`, '--paginate'], { encoding: 'utf8' }));
  table(releases.slice(0, 6).map((r) => ({ release: r.tag_name, downloads: r.assets.reduce((n, a) => n + a.download_count, 0) })));
} catch {
  console.log('  (gh not signed in)');
}

console.log('\nGoogle Analytics');
if (!process.env.GLASS_GA_KEY) {
  console.log('  Set GLASS_GA_KEY to a service account key file to include it.');
} else {
  const token = await gaToken(JSON.parse(readFileSync(process.env.GLASS_GA_KEY, 'utf8')));
  const report = async (body) => {
    const res = await fetch(`https://analyticsdata.googleapis.com/v1beta/${GA_PROPERTY}:runReport`, {
      method: 'POST',
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      body: JSON.stringify({ dateRanges: [{ startDate: `${days}daysAgo`, endDate: 'today' }], ...body }),
    });
    const d = await res.json();
    if (d.error) throw new Error(d.error.message);
    const names = [...(d.dimensionHeaders ?? []), ...d.metricHeaders].map((h) => h.name);
    return (d.rows ?? []).map((r) => Object.fromEntries([...(r.dimensionValues ?? []), ...r.metricValues].map((v, i) => [names[i], v.value])));
  };
  table(await report({ metrics: [{ name: 'totalUsers' }, { name: 'sessions' }, { name: 'screenPageViews' }] }));
  console.log('  Events');
  table(
    await report({
      dimensions: [{ name: 'eventName' }],
      metrics: [{ name: 'eventCount' }, { name: 'totalUsers' }],
      orderBys: [{ metric: { metricName: 'eventCount' }, desc: true }],
    }),
  );
  console.log('  Where visitors come from');
  table(
    await report({
      dimensions: [{ name: 'sessionSource' }],
      metrics: [{ name: 'totalUsers' }],
      orderBys: [{ metric: { metricName: 'totalUsers' }, desc: true }],
      limit: 10,
    }),
  );
}

/** A Google access token from a service account key (a signed JWT, no SDK). */
async function gaToken(key) {
  const b64 = (o) => Buffer.from(JSON.stringify(o)).toString('base64url');
  const now = Math.floor(Date.now() / 1000);
  const claims = { iss: key.client_email, scope: 'https://www.googleapis.com/auth/analytics.readonly', aud: 'https://oauth2.googleapis.com/token', iat: now, exp: now + 600 };
  const unsigned = `${b64({ alg: 'RS256', typ: 'JWT' })}.${b64(claims)}`;
  const sig = createSign('RSA-SHA256').update(unsigned).sign(key.private_key, 'base64url');
  const res = await fetch('https://oauth2.googleapis.com/token', {
    method: 'POST',
    body: new URLSearchParams({ grant_type: 'urn:ietf:params:oauth:grant-type:jwt-bearer', assertion: `${unsigned}.${sig}` }),
  });
  const d = await res.json();
  if (!d.access_token) throw new Error(`Google sign-in failed: ${d.error_description ?? d.error}`);
  return d.access_token;
}
