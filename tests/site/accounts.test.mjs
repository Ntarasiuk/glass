// Passkey accounts (/api/account), against the Worker under wrangler dev, with a software
// authenticator: the checks a real browser's passkeys can't easily get wrong on purpose.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const SITE = resolve(import.meta.dirname, '../../site');
const PORT = 8791;
const BASE = `http://localhost:${PORT}`;
let wrangler;

before(async () => {
  const persist = mkdtempSync(join(tmpdir(), 'glass-site-'));
  execFileSync('node', ['build.mjs'], { cwd: SITE, stdio: 'pipe' });
  execFileSync('npx', ['wrangler', 'd1', 'migrations', 'apply', 'getglass', '--local', '--persist-to', persist], { cwd: SITE, stdio: 'pipe' });
  wrangler = spawn('npx', ['wrangler', 'dev', '--port', String(PORT), '--local-upstream', `localhost:${PORT}`, '--persist-to', persist, '--var', 'VISITOR_SALT:test'], {
    cwd: SITE,
    stdio: 'ignore',
    detached: true,
  });
  for (let i = 0; i < 120; i++) {
    try {
      if ((await fetch(`${BASE}/llms.txt`)).ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error('wrangler dev did not start');
});

after(() => process.kill(-wrangler.pid));

const b64 = (bytes) => Buffer.from(bytes).toString('base64url');
const unb64 = (text) => new Uint8Array(Buffer.from(text, 'base64url'));
const sha256 = async (bytes) => new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
const concat = (...parts) => new Uint8Array(Buffer.concat(parts.map((p) => Buffer.from(p))));

/** A browser: keeps the session cookie, sends this site's Origin. */
class Browser {
  cookie = '';
  async call(path, { method = 'GET', body, origin = BASE } = {}) {
    const res = await fetch(`${BASE}${path}`, {
      method,
      headers: { origin, cookie: this.cookie, ...(body && { 'content-type': 'application/json' }) },
      body: body && JSON.stringify(body),
    });
    const set = res.headers.get('set-cookie');
    if (set) this.cookie = set.split(';')[0].endsWith('=') ? '' : set.split(';')[0];
    return { status: res.status, body: await res.json() };
  }
}

/** A software ES256 passkey. */
class Passkey {
  count = 0;
  static async make() {
    const k = new Passkey();
    k.pair = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
    k.id = b64(crypto.getRandomValues(new Uint8Array(16)));
    k.spki = b64(new Uint8Array(await crypto.subtle.exportKey('spki', k.pair.publicKey)));
    return k;
  }

  async authData({ rpId = 'localhost', flags = 0x05, attested = false } = {}) {
    const head = concat(await sha256(new TextEncoder().encode(rpId)), [flags | (attested ? 0x40 : 0)], new Uint8Array(new Uint32Array([0]).buffer));
    new DataView(head.buffer).setUint32(33, this.count);
    if (!attested) return head;
    const id = unb64(this.id);
    return concat(head, new Uint8Array(16), [id.length >> 8, id.length & 0xff], id);
  }

  clientData(type, challenge, origin = BASE) {
    return new TextEncoder().encode(JSON.stringify({ type, challenge, origin, crossOrigin: false }));
  }

  /** What the page posts after navigator.credentials.create. */
  async created(options, overrides = {}) {
    return {
      id: this.id,
      clientDataJSON: b64(this.clientData('webauthn.create', options.challenge, overrides.origin)),
      authenticatorData: b64(await this.authData({ attested: true, rpId: overrides.rpId })),
      publicKey: this.spki,
      publicKeyAlgorithm: -7,
    };
  }

  /** What the page posts after navigator.credentials.get. */
  async asserted(options, { origin, rpId, userHandle, tamper } = {}) {
    const clientData = this.clientData('webauthn.get', options.challenge, origin);
    const auth = await this.authData({ rpId });
    const raw = new Uint8Array(await crypto.subtle.sign({ name: 'ECDSA', hash: 'SHA-256' }, this.pair.privateKey, concat(auth, await sha256(clientData))));
    if (tamper) raw[5] ^= 1;
    return { id: this.id, clientDataJSON: b64(clientData), authenticatorData: b64(auth), signature: b64(rawToDer(raw)), userHandle };
  }
}

function rawToDer(raw) {
  const int = (v) => {
    let i = 0;
    while (i < v.length - 1 && v[i] === 0) i++;
    v = v.slice(i);
    return v[0] & 0x80 ? [0x02, v.length + 1, 0, ...v] : [0x02, v.length, ...v];
  };
  const body = [...int(raw.slice(0, 32)), ...int(raw.slice(32))];
  return new Uint8Array([0x30, body.length, ...body]);
}

async function newAccount() {
  const browser = new Browser();
  const key = await Passkey.make();
  const { body: options } = await browser.call('/api/account/challenge', { method: 'POST', body: { mode: 'create' } });
  const res = await browser.call('/api/account/passkey', { method: 'POST', body: await key.created(options) });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  return { browser, key, account: options.user.id };
}

async function signIn(browser, key, opts) {
  const { body: options } = await browser.call('/api/account/challenge', { method: 'POST', body: { mode: 'get' } });
  return browser.call('/api/account/session', { method: 'POST', body: await key.asserted(options, opts) });
}

test('a passkey makes an account and signs in; signing out, then in again, works', async () => {
  const { browser, key, account } = await newAccount();
  assert.match(browser.cookie, /^__Host-session=/);
  assert.equal((await browser.call('/api/account')).body.account.passkeys, 1);

  await browser.call('/api/account/session', { method: 'DELETE' });
  assert.equal(browser.cookie, '');
  assert.deepEqual((await browser.call('/api/account')).body, { account: null });

  const elsewhere = new Browser();
  assert.equal((await signIn(elsewhere, key, { userHandle: account })).status, 200);
  assert.equal((await elsewhere.call('/api/account')).body.account.passkeys, 1);
});

test('a challenge works once', async () => {
  const { key } = await newAccount();
  const browser = new Browser();
  const { body: options } = await browser.call('/api/account/challenge', { method: 'POST', body: { mode: 'get' } });
  const signed = await key.asserted(options);
  assert.equal((await browser.call('/api/account/session', { method: 'POST', body: signed })).status, 200);
  const again = await new Browser().call('/api/account/session', { method: 'POST', body: signed });
  assert.equal(again.status, 400);
  assert.match(again.body.error, /took too long/);
});

test('refuses another site, a bad signature, another account\'s handle, and cross-origin requests', async () => {
  const { key } = await newAccount();
  const cases = [
    [{ origin: 'https://evil.example' }, 400, /another site/],
    [{ rpId: 'evil.example' }, 400, /another site/],
    [{ tamper: true }, 401, /signature/],
    [{ userHandle: 'someone-else' }, 400, /another account/],
  ];
  for (const [opts, status, error] of cases) {
    const res = await signIn(new Browser(), key, opts);
    assert.equal(res.status, status, JSON.stringify(opts));
    assert.match(res.body.error, error);
  }
  // A page elsewhere can't use a signed-in browser's cookie to change anything.
  const forged = await new Browser().call('/api/account/challenge', { method: 'POST', body: { mode: 'get' }, origin: 'https://evil.example' });
  assert.equal(forged.status, 403);
});

test('a signed-in account adds a passkey; a signed-out browser can\'t add one to it', async () => {
  const { browser } = await newAccount();
  const second = await Passkey.make();
  const { body: options } = await browser.call('/api/account/challenge', { method: 'POST', body: { mode: 'create' } });
  assert.equal(options.excludeCredentials.length, 1);
  assert.equal((await browser.call('/api/account/passkey', { method: 'POST', body: await second.created(options) })).status, 200);
  assert.equal((await browser.call('/api/account')).body.account.passkeys, 2);

  // Signed out between the challenge and the passkey: refused.
  const third = await Passkey.make();
  const { body: late } = await browser.call('/api/account/challenge', { method: 'POST', body: { mode: 'create' } });
  await browser.call('/api/account/session', { method: 'DELETE' });
  const res = await browser.call('/api/account/passkey', { method: 'POST', body: await third.created(late) });
  assert.equal(res.status, 403);
});

test('a passkey whose counter went backwards is refused (a copy)', async () => {
  const { key } = await newAccount();
  key.count = 5;
  assert.equal((await signIn(new Browser(), key)).status, 200);
  key.count = 3;
  const res = await signIn(new Browser(), key);
  assert.equal(res.status, 401);
  assert.match(res.body.error, /copied/);
});

test('deleting the account leaves its passkey unknown', async () => {
  const { browser, key } = await newAccount();
  assert.equal((await browser.call('/api/account', { method: 'DELETE' })).status, 200);
  assert.deepEqual((await browser.call('/api/account')).body, { account: null });
  const res = await signIn(new Browser(), key);
  assert.equal(res.status, 400);
  assert.equal(res.body.unknown, true);
});

/** A Glass host's key, signing like `glass account` does (raw P-256 point, P1363 signatures). */
async function hostKey() {
  const pair = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
  const key = b64(new Uint8Array(await crypto.subtle.exportKey('raw', pair.publicKey)));
  const sign = async (text) => b64(new Uint8Array(await crypto.subtle.sign({ name: 'ECDSA', hash: 'SHA-256' }, pair.privateKey, new TextEncoder().encode(text))));
  return {
    key,
    async claim({ name = 'Studio', ts = Math.floor(Date.now() / 1000), signedName = name } = {}) {
      const code = b64(crypto.getRandomValues(new Uint8Array(32)));
      const sig = await sign(`glass-claim-v1\n${code}\n${ts}\n${signedName}`);
      const res = await fetch(`${BASE}/api/claims`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ key, code, ts, name, sig }) });
      return { status: res.status, body: await res.json(), code };
    },
    async remove({ ts = Math.floor(Date.now() / 1000), other } = {}) {
      const sig = await (other ?? this).signRemove(ts);
      const res = await fetch(`${BASE}/api/claims/remove`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ key, ts, sig }) });
      return { status: res.status, body: await res.json() };
    },
    signRemove: (ts) => sign(`glass-unclaim-v1\n${ts}`),
    /** A fleet report: `data` is sent as given (a string), signed as sent. */
    async report(data, { ts = Math.floor(Date.now() / 1000), signedData = data } = {}) {
      const sig = await sign(`glass-fleet-v1\n${ts}\n${signedData}`);
      const res = await fetch(`${BASE}/api/fleet/report`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ key, ts, data, sig }) });
      return { status: res.status, body: await res.json() };
    },
  };
}

test('a computer joins an account only by a claim its host signed, fresh, used once', async () => {
  const host = await hostKey();
  assert.equal((await host.claim({ signedName: 'Something else' })).status, 401);
  assert.equal((await host.claim({ ts: Math.floor(Date.now() / 1000) - 3600 })).status, 400);

  const { status, body, code } = await host.claim();
  assert.equal(status, 200);
  assert.equal(body.url, `${BASE}/computers#claim=${code}`);

  // Signed out, or from another site: no.
  assert.equal((await new Browser().call('/api/account/computers', { method: 'POST', body: { claim: code } })).status, 401);
  const { browser } = await newAccount();
  assert.equal((await browser.call('/api/account/computers', { method: 'POST', body: { claim: code }, origin: 'https://evil.example' })).status, 403);

  assert.deepEqual((await browser.call(`/api/account/claim?code=${code}`)).body, { name: 'Studio' });
  assert.deepEqual((await browser.call('/api/account/computers', { method: 'POST', body: { claim: code } })).body, { name: 'Studio' });
  const { body: list } = await browser.call('/api/account/computers');
  assert.equal(list.computers.length, 1);
  assert.equal(list.computers[0].key, host.key);
  assert.equal(list.computers[0].online, false);

  // Used: not again, not by anyone.
  const { browser: other } = await newAccount();
  assert.equal((await other.call('/api/account/computers', { method: 'POST', body: { claim: code } })).status, 404);
  assert.equal((await other.call('/api/account/computers')).body.computers.length, 0);
});

test('only the computer itself takes itself off accounts', async () => {
  const host = await hostKey();
  const { browser } = await newAccount();
  const { code } = await host.claim();
  await browser.call('/api/account/computers', { method: 'POST', body: { claim: code } });

  const stranger = await hostKey();
  assert.equal((await host.remove({ other: stranger })).status, 401);
  assert.equal((await browser.call('/api/account/computers')).body.computers.length, 1);

  assert.deepEqual((await host.remove()).body, { removed: 1 });
  assert.equal((await browser.call('/api/account/computers')).body.computers.length, 0);
});

test('deleting an account deletes its list of computers', async () => {
  const host = await hostKey();
  const { browser, key } = await newAccount();
  const { code } = await host.claim();
  await browser.call('/api/account/computers', { method: 'POST', body: { claim: code } });
  await browser.call('/api/account', { method: 'DELETE' });
  assert.deepEqual((await host.remove()).body, { removed: 0 });
  assert.equal((await signIn(new Browser(), key)).body.unknown, true);
});

const REPORT = { os: 'macOS 15.1', model: 'MacBook Pro', chip: 'Apple M3 Max', ram_gb: 64, disk_total_gb: 994, disk_free_gb: 412, filevault: true, firewall: false, uptime_h: 52, battery: 83, version: '0.9.0' };

test('a computer on an account reports about itself, signed, and only its account sees it', async () => {
  const host = await hostKey();
  // Not on an account: nothing is kept.
  assert.equal((await host.report(JSON.stringify(REPORT))).status, 403);

  const { browser } = await newAccount();
  const { code } = await host.claim();
  await browser.call('/api/account/computers', { method: 'POST', body: { claim: code } });

  // Not signed by that computer, or stale: refused.
  assert.equal((await host.report(JSON.stringify(REPORT), { signedData: '{}' })).status, 401);
  assert.equal((await host.report(JSON.stringify(REPORT), { ts: Math.floor(Date.now() / 1000) - 3600 })).status, 400);
  assert.equal((await host.report('not json')).status, 400);

  assert.equal((await host.report(JSON.stringify(REPORT))).status, 200);
  const { computers } = (await browser.call('/api/account/computers')).body;
  assert.equal(computers[0].report.model, 'MacBook Pro');
  assert.equal(computers[0].report.filevault, true);
  assert.equal(computers[0].report.firewall, false);
  assert.equal(computers[0].report.battery, 83);
  assert.ok(computers[0].report.at > Date.now() - 60_000);

  // Another account sees nothing of it.
  const { browser: other } = await newAccount();
  assert.equal((await other.call('/api/account/computers')).body.computers.length, 0);

  // At most one every few seconds.
  assert.equal((await host.report(JSON.stringify(REPORT))).status, 429);
});

test('a report keeps only the fields it may, each the right kind and size', async () => {
  const host = await hostKey();
  const { browser } = await newAccount();
  const { code } = await host.claim();
  await browser.call('/api/account/computers', { method: 'POST', body: { claim: code } });
  const data = JSON.stringify({ ...REPORT, os: 'x'.repeat(500), model: 'Mac\u0007Book', secret: 'nope', ram_gb: 'lots', battery: -5, filevault: 'yes' });
  assert.equal((await host.report(data)).status, 200);
  const r = (await browser.call('/api/account/computers')).body.computers[0].report;
  assert.equal(r.os.length, 80);
  assert.equal(r.model, 'MacBook');
  assert.equal(r.secret, undefined);
  assert.equal(r.ram_gb, undefined);
  assert.equal(r.battery, undefined);
  assert.equal(r.filevault, undefined);
  assert.equal((await host.report('x'.repeat(5000))).status, 400);
});

test('a report goes when the computer asks, leaves its account, or the account is deleted', async () => {
  const host = await hostKey();
  const { browser } = await newAccount();
  const { code } = await host.claim();
  await browser.call('/api/account/computers', { method: 'POST', body: { claim: code } });
  await host.report(JSON.stringify(REPORT));
  // Empty data, signed, deletes it.
  assert.deepEqual((await host.report('')).body, { deleted: true });
  assert.equal((await browser.call('/api/account/computers')).body.computers[0].report, undefined);

  // Taken off the account (by the account, then again by the computer): the report isn't kept.
  await new Promise((r) => setTimeout(r, 100));
  const host2 = await hostKey();
  const { browser: b2 } = await newAccount();
  const c2 = await host2.claim();
  await b2.call('/api/account/computers', { method: 'POST', body: { claim: c2.code } });
  await host2.report(JSON.stringify(REPORT));
  const id = (await b2.call('/api/account/computers')).body.computers[0].id;
  await b2.call(`/api/account/computers/${id}`, { method: 'DELETE' });
  assert.equal((await host2.report(JSON.stringify(REPORT))).status, 403);
});

test('the signed-in page gets its passkeys\' keys, to vouch for one when it lets the account manage a computer', async () => {
  const { browser } = await newAccount();
  const { keys } = (await browser.call('/api/account')).body.account;
  assert.equal(keys.length, 1);
  assert.equal(keys[0].alg, -7);
  assert.match(keys[0].key, /^[A-Za-z0-9_-]{120,}$/);
  assert.equal((await new Browser().call('/api/account')).body.account, null);
});

test('an action or enrollment goes only to a computer on the signed-in account, in a sane size', async () => {
  const host = await hostKey();
  const { browser } = await newAccount();
  const { code } = await host.claim();
  await browser.call('/api/account/computers', { method: 'POST', body: { claim: code } });
  const id = (await browser.call('/api/account/computers')).body.computers[0].id;
  const assertion = { id: 'a', clientDataJSON: 'b', authenticatorData: 'c', signature: 'd' };

  // Signed out, another site, someone else's computer, bad shapes: refused before the computer hears of it.
  assert.equal((await new Browser().call(`/api/account/computers/${id}/action`, { method: 'POST', body: { cmd: '{}', assertion } })).status, 401);
  assert.equal((await browser.call(`/api/account/computers/${id}/action`, { method: 'POST', body: { cmd: '{}', assertion }, origin: 'https://evil.example' })).status, 403);
  const { browser: other } = await newAccount();
  assert.equal((await other.call(`/api/account/computers/${id}/action`, { method: 'POST', body: { cmd: '{}', assertion } })).status, 404);
  assert.equal((await browser.call(`/api/account/computers/${id}/action`, { method: 'POST', body: { cmd: '{}' } })).status, 400);
  assert.equal((await browser.call(`/api/account/computers/${id}/action`, { method: 'POST', body: { cmd: 'x'.repeat(2000), assertion } })).status, 400);
  assert.equal((await browser.call(`/api/account/computers/${id}/manage`, { method: 'POST', body: { cred: 'a' } })).status, 400);

  // A well-formed one for a computer that isn't connected: it says so.
  const offline = await browser.call(`/api/account/computers/${id}/action`, { method: 'POST', body: { cmd: '{}', assertion } });
  assert.equal(offline.status, 409);
  assert.match(offline.body.error, /offline/);
});

test('an account keeps a policy: only its own fields, sized and typed, its own, gone with the account', async () => {
  const { browser, key } = await newAccount();
  assert.deepEqual((await browser.call('/api/account/policy')).body.policy, { filevault: false, firewall: false, glass_current: false, disk_free_gb: 0, fresh_hours: 0, os_min: '' });
  assert.equal((await new Browser().call('/api/account/policy')).status, 401);
  assert.equal((await new Browser().call('/api/account/policy', { method: 'PUT', body: {} })).status, 401);
  assert.equal((await browser.call('/api/account/policy', { method: 'PUT', body: { filevault: true }, origin: 'https://evil.example' })).status, 403);

  const saved = await browser.call('/api/account/policy', { method: 'PUT', body: { filevault: true, firewall: 'yes', glass_current: true, disk_free_gb: 50.4, fresh_hours: -3, os_min: '15.1', extra: 'nope' } });
  assert.deepEqual(saved.body.policy, { filevault: true, firewall: false, glass_current: true, disk_free_gb: 50, fresh_hours: 0, os_min: '15.1' });
  assert.deepEqual((await browser.call('/api/account/policy')).body.policy, saved.body.policy);
  // A bad version is dropped, huge numbers are capped.
  const odd = (await browser.call('/api/account/policy', { method: 'PUT', body: { os_min: '15; drop table', disk_free_gb: 1e12, fresh_hours: 1e9 } })).body.policy;
  assert.equal(odd.os_min, '');
  assert.equal(odd.disk_free_gb, 100000);
  assert.equal(odd.fresh_hours, 8760);

  // Another account has its own.
  const { browser: other } = await newAccount();
  assert.equal((await other.call('/api/account/policy')).body.policy.filevault, false);

  // Gone with the account.
  await browser.call('/api/account/policy', { method: 'PUT', body: { filevault: true } });
  await browser.call('/api/account', { method: 'DELETE' });
  assert.equal((await signIn(new Browser(), key)).body.unknown, true);
});
