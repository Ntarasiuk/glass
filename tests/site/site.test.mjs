// Runs the getglass Worker locally (wrangler dev, fresh local D1) and checks every route.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const SITE = resolve(import.meta.dirname, '../../site');
const PORT = 8790;
const BASE = `http://localhost:${PORT}`;
const KEY = 'test-stats-key';
let wrangler;

before(async () => {
  const persist = mkdtempSync(join(tmpdir(), 'glass-site-'));
  const run = (args) => execFileSync('npx', ['wrangler', ...args, '--persist-to', persist], { cwd: SITE, stdio: 'pipe' });
  execFileSync('node', ['build.mjs'], { cwd: SITE, stdio: 'pipe' });
  run(['d1', 'migrations', 'apply', 'getglass', '--local']);
  wrangler = spawn(
    'npx',
    ['wrangler', 'dev', '--port', String(PORT), '--persist-to', persist, '--var', `STATS_KEY:${KEY}`, '--var', 'VISITOR_SALT:test'],
    { cwd: SITE, stdio: 'ignore', detached: true },
  );
  for (let i = 0; i < 120; i++) {
    try {
      if ((await fetch(`${BASE}/llms.txt`)).ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error('wrangler dev did not start');
});

after(() => process.kill(-wrangler.pid));

const get = (path, ua) => fetch(`${BASE}${path}`, { headers: ua ? { 'user-agent': ua } : {}, redirect: 'manual' });
const feedback = (body) =>
  fetch(`${BASE}/api/feedback`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
const statsFor = async () => (await fetch(`${BASE}/api/stats`, { headers: { authorization: `Bearer ${KEY}` } })).json();

test('command-line fetchers get the install script at /', async () => {
  for (const ua of ['curl/8.7.1', 'Wget/1.21']) {
    const res = await get('/', ua);
    assert.equal(res.status, 200);
    assert.match(res.headers.get('content-type'), /shellscript/);
    assert.match(await res.text(), /^#!\/bin\/sh/);
  }
});

test('browsers get the landing page with structured data', async () => {
  const html = await (await get('/', 'Mozilla/5.0 (Macintosh) Safari/605.1.15')).text();
  assert.match(html, /<title>Glass/);
  assert.match(html, /"@type": "SoftwareApplication"/);
  assert.match(html, /curl -fsSL glassdesk\.dev \| sh/);
});

test('docs exist in every format and agree', async () => {
  const md = await (await get('/docs.md')).text();
  const full = await (await get('/llms-full.txt')).text();
  assert.equal(md, full);
  const llms = await (await get('/llms.txt')).text();
  assert.match(llms, /^# Glass\n\n> /);
  assert.match(llms, /llms-full\.txt/);
  assert.equal((await get('/docs/')).status, 200);
  const skill = await (await get('/skill.md')).text();
  assert.match(skill, /^---\nname: glass\ndescription: /);
  assert.match(skill, /glass setup --yes --json/);
  const robots = await (await get('/robots.txt')).text();
  for (const bot of ['GPTBot', 'ClaudeBot', 'PerplexityBot']) assert.match(robots, new RegExp(bot));
});

const hasReleases = existsSync(join(SITE, 'public/releases/glass-latest-darwin-universal.tar.gz'));

test('release downloads are served with checksums and counted', { skip: !hasReleases && 'no release built (scripts/package.sh)' }, async () => {
  const before = await statsFor();
  const count = (s, kind) => s.downloads.find((d) => d.kind === kind)?.total ?? 0;
  for (const platform of ['darwin-universal', 'linux-x86_64']) {
    const res = await get(`/releases/glass-latest-${platform}.tar.gz`);
    assert.equal(res.status, 200, platform);
    const sum = await (await get(`/releases/glass-latest-${platform}.tar.gz.sha256`)).text();
    assert.match(sum, /^[0-9a-f]{64}  glass-latest-/);
  }
  await get('/install.sh');
  await new Promise((r) => setTimeout(r, 300));
  const after = await statsFor();
  assert.equal(count(after, 'release') - count(before, 'release'), 2);
  assert.ok(count(after, 'script') > count(before, 'script'));
});

test('feedback is validated', async () => {
  assert.equal((await feedback({ message: '' })).status, 400);
  assert.equal((await feedback({ message: 'x'.repeat(4001) })).status, 400);
  assert.equal((await feedback({ message: 'hi', email: 'not-an-email' })).status, 400);
  assert.equal((await fetch(`${BASE}/api/feedback`, { method: 'POST', body: '{nope' })).status, 400);
  assert.equal((await fetch(`${BASE}/api/feedback`)).status, 405);
});

test('feedback is stored, CORS-enabled and rate limited', async () => {
  const pre = await fetch(`${BASE}/api/feedback`, { method: 'OPTIONS' });
  assert.equal(pre.status, 204);
  assert.equal(pre.headers.get('access-control-allow-origin'), '*');
  const ok = await feedback({ message: 'love it', email: 'me@example.com', source: 'test', version: '0.1.0' });
  assert.equal(ok.status, 200);
  const latest = (await statsFor()).feedback[0];
  assert.equal(latest.message, 'love it');
  assert.equal(latest.email, 'me@example.com');
  let status;
  for (let i = 0; i < 6; i++) status = (await feedback({ message: `spam ${i}` })).status;
  assert.equal(status, 429);
});

test('stats need the key', async () => {
  assert.equal((await fetch(`${BASE}/api/stats`)).status, 401);
  assert.equal((await fetch(`${BASE}/api/stats`, { headers: { authorization: 'Bearer wrong' } })).status, 401);
});
