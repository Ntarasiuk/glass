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
  assert.deepEqual((await browser.call('/api/account')).body, { account: { passkeys: 1 } });

  await browser.call('/api/account/session', { method: 'DELETE' });
  assert.equal(browser.cookie, '');
  assert.deepEqual((await browser.call('/api/account')).body, { account: null });

  const elsewhere = new Browser();
  assert.equal((await signIn(elsewhere, key, { userHandle: account })).status, 200);
  assert.deepEqual((await elsewhere.call('/api/account')).body, { account: { passkeys: 1 } });
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
  assert.deepEqual((await browser.call('/api/account')).body, { account: { passkeys: 2 } });

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
