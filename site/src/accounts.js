// Accounts at glassdesk.dev/computers: a passkey and nothing else (no email, no password).
// Passkeys sync (iCloud Keychain, Google Password Manager…), so every device you own signs
// in. An account only lists computers: getting into one still takes that computer's own
// sign-in, so a taken-over account shows names and who's online, never a screen.
//
// WebAuthn without attestation (any authenticator), discoverable credentials (signing in
// needs no name), ES256 or RS256, checked here with WebCrypto.

import { fromB64url, toB64url } from './introducer.js';
import { visitorId } from './index.js';

const SESSION_COOKIE = '__Host-session';
const SESSION_MS = 30 * 86400_000;
const CHALLENGE_MS = 5 * 60_000;
// Open challenges a visitor may hold at once (each lasts CHALLENGE_MS).
const OPEN_CHALLENGES = 20;
const PASSKEYS_PER_ACCOUNT = 20;
const ES256 = -7;
const RS256 = -257;
const USER_NAME = 'Your computers';

/** Routes under /api/account; `null` for anything else. */
export async function accounts(request, env, url) {
  const path = url.pathname;
  if (path !== '/api/account' && !path.startsWith('/api/account/')) return null;
  const { method } = request;
  // Changes only from this site's own pages (the cookie is SameSite=Strict too).
  if (method !== 'GET' && request.headers.get('origin') !== url.origin) return json({ error: 'cross-origin request' }, 403);
  try {
    if (path === '/api/account' && method === 'GET') return await me(request, env);
    if (path === '/api/account' && method === 'DELETE') return await deleteAccount(request, env);
    if (path === '/api/account/challenge' && method === 'POST') return await challenge(request, env, url);
    if (path === '/api/account/passkey' && method === 'POST') return await register(request, env, url);
    if (path === '/api/account/session' && method === 'POST') return await signIn(request, env, url);
    if (path === '/api/account/session' && method === 'DELETE') return await signOut(request, env);
  } catch (e) {
    if (e instanceof Refused) return json({ error: e.message, ...e.extra }, e.status);
    throw e;
  }
  return json({ error: 'not found' }, 404);
}

/** The signed-in account, from the session cookie: `{ account, token }` or null. */
export async function session(request, env) {
  const token = cookie(request, SESSION_COOKIE);
  if (!token) return null;
  const row = await env.DB.prepare('SELECT account FROM sessions WHERE token_hash = ? AND expires > ?')
    .bind(await sha256b64(token), Date.now())
    .first();
  return row ? { account: row.account, token } : null;
}

class Refused extends Error {
  constructor(message, status = 400, extra = {}) {
    super(message);
    this.status = status;
    this.extra = extra;
  }
}

async function me(request, env) {
  const s = await session(request, env);
  if (!s) return json({ account: null });
  const passkeys = await env.DB.prepare('SELECT COUNT(*) AS n FROM passkeys WHERE account = ?').bind(s.account).first('n');
  return json({ account: { passkeys } });
}

/** Options for navigator.credentials.create (a new account, or another passkey for this one) or .get. */
async function challenge(request, env, url) {
  const { mode } = await body(request);
  if (mode !== 'create' && mode !== 'get') throw new Refused('mode: create or get');
  const now = Date.now();
  const visitor = await visitorId(request, env);
  await env.DB.prepare('DELETE FROM account_challenges WHERE expires < ?').bind(now).run();
  const open = await env.DB.prepare('SELECT COUNT(*) AS n FROM account_challenges WHERE visitor = ?').bind(visitor).first('n');
  if (open >= OPEN_CHALLENGES) throw new Refused('Too many tries. Wait a few minutes.', 429);
  const signedIn = mode === 'create' ? await session(request, env) : null;
  const account = mode === 'create' ? (signedIn?.account ?? randomB64(16)) : null;
  const value = randomB64(32);
  await env.DB.prepare('INSERT INTO account_challenges (challenge, mode, account, expires, visitor) VALUES (?, ?, ?, ?, ?)')
    .bind(value, mode, account, now + CHALLENGE_MS, visitor)
    .run();
  if (mode === 'get') return json({ challenge: value, rpId: url.hostname, userVerification: 'preferred', timeout: CHALLENGE_MS });
  const existing = signedIn
    ? (await env.DB.prepare('SELECT id FROM passkeys WHERE account = ?').bind(account).all()).results.map((r) => ({ type: 'public-key', id: r.id }))
    : [];
  return json({
    challenge: value,
    rp: { id: url.hostname, name: 'Glass' },
    user: { id: account, name: USER_NAME, displayName: USER_NAME },
    pubKeyCredParams: [ES256, RS256].map((alg) => ({ type: 'public-key', alg })),
    authenticatorSelection: { residentKey: 'required', userVerification: 'preferred' },
    attestation: 'none',
    excludeCredentials: existing,
    timeout: CHALLENGE_MS,
  });
}

/** A new passkey: for a new account (signing it in), or added to the signed-in one. */
async function register(request, env, url) {
  const cred = await body(request);
  const client = clientData(cred.clientDataJSON, 'webauthn.create', url);
  const account = await takeChallenge(env, client.challenge, 'create');
  const auth = await authenticatorData(cred.authenticatorData, url);
  if (!(auth.flags & 0x40) || auth.credentialId !== cred.id) throw new Refused('The passkey and its data disagree.');
  const alg = Number(cred.publicKeyAlgorithm);
  if (alg !== ES256 && alg !== RS256) throw new Refused('This passkey uses a kind of key Glass doesn\'t take. Try another password manager.');
  await importKey(alg, String(cred.publicKey ?? '')).catch(() => {
    throw new Refused('The passkey\'s public key is unreadable.');
  });
  const now = Date.now();
  const exists = await env.DB.prepare('SELECT 1 FROM accounts WHERE id = ?').bind(account).first();
  if (exists) {
    // Another passkey for an account: only while it's still signed in here.
    if ((await session(request, env))?.account !== account) throw new Refused('Sign in again first.', 403);
    const n = await env.DB.prepare('SELECT COUNT(*) AS n FROM passkeys WHERE account = ?').bind(account).first('n');
    if (n >= PASSKEYS_PER_ACCOUNT) throw new Refused('This account has as many passkeys as it can hold.');
  }
  const insert = env.DB.prepare('INSERT INTO passkeys (id, account, public_key, alg, sign_count, created) VALUES (?, ?, ?, ?, ?, ?)')
    .bind(cred.id, account, cred.publicKey, alg, auth.signCount, now);
  try {
    await env.DB.batch(exists ? [insert] : [env.DB.prepare('INSERT INTO accounts (id, created) VALUES (?, ?)').bind(account, now), insert]);
  } catch {
    throw new Refused('That passkey is already on an account.', 409);
  }
  if (exists) return json({ ok: true });
  return withSession(json({ ok: true }), await startSession(env, account));
}

async function signIn(request, env, url) {
  const cred = await body(request);
  const client = clientData(cred.clientDataJSON, 'webauthn.get', url);
  await takeChallenge(env, client.challenge, 'get');
  const passkey = await env.DB.prepare('SELECT account, public_key, alg, sign_count FROM passkeys WHERE id = ?').bind(String(cred.id ?? '')).first();
  // The page tells the password manager to drop a passkey whose account is gone.
  if (!passkey) throw new Refused('That passkey isn\'t on any account. It may have been deleted.', 400, { unknown: true });
  if (cred.userHandle && cred.userHandle !== passkey.account) throw new Refused('That passkey belongs to another account.');
  const auth = await authenticatorData(cred.authenticatorData, url);
  const signed = concat(fromB64url(cred.authenticatorData), new Uint8Array(await crypto.subtle.digest('SHA-256', fromB64url(cred.clientDataJSON))));
  if (!(await verify(passkey.alg, passkey.public_key, String(cred.signature ?? ''), signed))) throw new Refused('The passkey\'s signature doesn\'t check out.', 401);
  // A counter that went backwards is a cloned authenticator (synced passkeys stay at 0).
  if ((auth.signCount || passkey.sign_count) && auth.signCount <= passkey.sign_count) throw new Refused('This passkey looks copied. Use another one.', 401);
  await env.DB.prepare('UPDATE passkeys SET sign_count = ?, used = ? WHERE id = ?').bind(auth.signCount, Date.now(), cred.id).run();
  return withSession(json({ ok: true }), await startSession(env, passkey.account));
}

async function signOut(request, env) {
  const s = await session(request, env);
  if (s) await env.DB.prepare('DELETE FROM sessions WHERE token_hash = ?').bind(await sha256b64(s.token)).run();
  return clearSession(json({ ok: true }));
}

/** Everything about the account, gone. */
async function deleteAccount(request, env) {
  const s = await session(request, env);
  if (!s) throw new Refused('Not signed in.', 401);
  await env.DB.batch(['passkeys', 'sessions'].map((t) => env.DB.prepare(`DELETE FROM ${t} WHERE account = ?`).bind(s.account)).concat(
    env.DB.prepare('DELETE FROM accounts WHERE id = ?').bind(s.account),
  ));
  return clearSession(json({ ok: true }));
}

/** Uses up a challenge this site made: the account it was made for (create), or ''. */
async function takeChallenge(env, value, mode) {
  const row = await env.DB.prepare('DELETE FROM account_challenges WHERE challenge = ? AND mode = ? AND expires > ? RETURNING account')
    .bind(String(value), mode, Date.now())
    .first();
  if (!row) throw new Refused('That took too long. Try again.');
  return row.account ?? '';
}

function clientData(b64, type, url) {
  let data;
  try {
    data = JSON.parse(new TextDecoder().decode(fromB64url(String(b64))));
  } catch {
    throw new Refused('Unreadable passkey response.');
  }
  if (data.type !== type || data.origin !== url.origin || data.crossOrigin === true) throw new Refused('That passkey response is for another site.');
  return data;
}

/** The parts of authenticator data that matter here, after checking it's for this site and the person was there. */
async function authenticatorData(b64, url) {
  let bytes;
  try {
    bytes = fromB64url(String(b64));
  } catch {
    throw new Refused('Unreadable passkey response.');
  }
  if (bytes.length < 37) throw new Refused('Unreadable passkey response.');
  const rpIdHash = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(url.hostname)));
  if (!equal(bytes.slice(0, 32), rpIdHash)) throw new Refused('That passkey is for another site.');
  const flags = bytes[32];
  if (!(flags & 0x01)) throw new Refused('The passkey didn\'t check you were there.');
  const signCount = new DataView(bytes.buffer, bytes.byteOffset + 33, 4).getUint32(0);
  // Attested credential data (registration): AAGUID (16), id length (2), id.
  let credentialId;
  if (flags & 0x40 && bytes.length >= 55) {
    const len = (bytes[53] << 8) | bytes[54];
    credentialId = toB64url(bytes.slice(55, 55 + len));
  }
  return { flags, signCount, credentialId };
}

function importKey(alg, spki) {
  const algorithm = alg === ES256 ? { name: 'ECDSA', namedCurve: 'P-256' } : { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' };
  return crypto.subtle.importKey('spki', fromB64url(spki), algorithm, false, ['verify']);
}

async function verify(alg, spki, signatureB64, data) {
  try {
    const key = await importKey(alg, spki);
    const signature = fromB64url(signatureB64);
    if (alg === ES256) return await crypto.subtle.verify({ name: 'ECDSA', hash: 'SHA-256' }, key, derToRaw(signature), data);
    return await crypto.subtle.verify('RSASSA-PKCS1-v1_5', key, signature, data);
  } catch {
    return false;
  }
}

/** WebAuthn's ECDSA signatures are DER (SEQUENCE of two INTEGERs); WebCrypto wants r ‖ s. */
export function derToRaw(der) {
  if (der[0] !== 0x30) throw new Error('not DER');
  let i = 2;
  const int = () => {
    if (der[i++] !== 0x02) throw new Error('not DER');
    const len = der[i++];
    let v = der.slice(i, i + len);
    i += len;
    while (v.length > 32 && v[0] === 0) v = v.slice(1);
    if (v.length > 32) throw new Error('not P-256');
    const out = new Uint8Array(32);
    out.set(v, 32 - v.length);
    return out;
  };
  return concat(int(), int());
}

async function startSession(env, account) {
  const token = randomB64(32);
  const now = Date.now();
  await env.DB.batch([
    env.DB.prepare('DELETE FROM sessions WHERE account = ? AND expires < ?').bind(account, now),
    env.DB.prepare('INSERT INTO sessions (token_hash, account, created, expires) VALUES (?, ?, ?, ?)').bind(await sha256b64(token), account, now, now + SESSION_MS),
  ]);
  return token;
}

function withSession(res, token) {
  res.headers.append('set-cookie', `${SESSION_COOKIE}=${token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=${SESSION_MS / 1000}`);
  return res;
}

function clearSession(res) {
  res.headers.append('set-cookie', `${SESSION_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0`);
  return res;
}

function cookie(request, name) {
  for (const part of (request.headers.get('cookie') ?? '').split(';')) {
    const [k, ...v] = part.trim().split('=');
    if (k === name) return v.join('=');
  }
  return undefined;
}

async function body(request) {
  try {
    const b = await request.json();
    if (b && typeof b === 'object') return b;
  } catch {}
  throw new Refused('expected JSON');
}

async function sha256b64(text) {
  return toB64url(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))));
}

function randomB64(bytes) {
  return toB64url(crypto.getRandomValues(new Uint8Array(bytes)));
}

function concat(...parts) {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

function equal(a, b) {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

function json(data, status = 200) {
  return new Response(JSON.stringify(data), { status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });
}
