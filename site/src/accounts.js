// Accounts at glassdesk.dev/computers: a passkey and nothing else (no email, no password).
// Passkeys sync (iCloud Keychain, Google Password Manager…), so every device you own signs
// in. An account only lists computers: getting into one still takes that computer's own
// sign-in, so a taken-over account shows names and who's online, never a screen.
//
// WebAuthn without attestation (any authenticator), discoverable credentials (signing in
// needs no name), ES256 or RS256, checked here with WebCrypto.
//
// A computer joins an account only through a claim its host signed (`glass account add`):
// a one-time code in a link, used here by a signed-in browser.

import { fromB64url, hostsOnline, toB64url, verifySigned } from './introducer.js';
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
const CLAIM_MS = 15 * 60_000;
// A host's signed claim must be this fresh (its clock may be off a little).
const CLAIM_CLOCK_MS = 5 * 60_000;
const CLAIMS_PER_HOUR = 20;
const COMPUTERS_PER_ACCOUNT = 32;
const NAME_MAX = 64;

/** Routes under /api/account and /api/claims; `null` for anything else. */
export async function accounts(request, env, url) {
  const path = url.pathname;
  const { method } = request;
  // From hosts (`glass account`), each request signed with the host's key.
  if (path === '/api/claims' || path === '/api/claims/remove' || path === '/api/fleet/report') {
    if (method !== 'POST') return json({ error: 'method not allowed' }, 405);
    try {
      if (path === '/api/fleet/report') return await fleetReport(request, env);
      return await (path === '/api/claims' ? makeClaim(request, env, url) : unclaim(request, env));
    } catch (e) {
      if (e instanceof Refused) return json({ error: e.message }, e.status);
      throw e;
    }
  }
  if (path !== '/api/account' && !path.startsWith('/api/account/')) return null;
  // Changes only from this site's own pages (the cookie is SameSite=Strict too).
  if (method !== 'GET' && request.headers.get('origin') !== url.origin) return json({ error: 'cross-origin request' }, 403);
  try {
    if (path === '/api/account' && method === 'GET') return await me(request, env);
    if (path === '/api/account' && method === 'DELETE') return await deleteAccount(request, env);
    if (path === '/api/account/challenge' && method === 'POST') return await challenge(request, env, url);
    if (path === '/api/account/passkey' && method === 'POST') return await register(request, env, url);
    if (path === '/api/account/session' && method === 'POST') return await signIn(request, env, url);
    if (path === '/api/account/session' && method === 'DELETE') return await signOut(request, env);
    if (path === '/api/account/claim' && method === 'GET') return await peekClaim(env, url);
    if (path === '/api/account/policy' && method === 'GET') return await getPolicy(request, env);
    if (path === '/api/account/policy' && method === 'PUT') return await putPolicy(request, env);
    if (path === '/api/account/computers' && method === 'GET') return await computers(request, env);
    if (path === '/api/account/computers' && method === 'POST') return await addComputer(request, env);
    const remove = path.match(/^\/api\/account\/computers\/([A-Za-z0-9_-]{22})$/);
    if (remove && method === 'DELETE') return await removeComputer(request, env, remove[1]);
    const relay = path.match(/^\/api\/account\/computers\/([A-Za-z0-9_-]{22})\/(action|manage)$/);
    if (relay && method === 'POST') return await relayToComputer(request, env, relay[1], relay[2]);
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
  const { results } = await env.DB.prepare('SELECT id, public_key, alg FROM passkeys WHERE account = ?').bind(s.account).all();
  // The keys are for the page to vouch for a passkey when it lets an account manage a computer.
  return json({ account: { passkeys: results.length, keys: results.map((k) => ({ id: k.id, key: k.public_key, alg: k.alg })) } });
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
  await env.DB.batch(['passkeys', 'sessions', 'account_computers', 'account_policy'].map((t) => env.DB.prepare(`DELETE FROM ${t} WHERE account = ?`).bind(s.account)).concat(
    env.DB.prepare('DELETE FROM accounts WHERE id = ?').bind(s.account),
    dropOrphanReports(env),
  ));
  return clearSession(json({ ok: true }));
}

/** A host asks for a one-time code that adds it to whichever account uses it. */
async function makeClaim(request, env, url) {
  const { key, code, ts, name, sig } = await body(request);
  const clean = String(name ?? '').replace(/[\u0000-\u001f\u007f]/g, '').trim().slice(0, NAME_MAX);
  if (!clean || !/^[A-Za-z0-9_-]{43}$/.test(String(code))) throw new Refused('expected key, code, ts, name and sig');
  if (Math.abs(Date.now() - Number(ts) * 1000) > CLAIM_CLOCK_MS) throw new Refused('This computer\'s clock is off by more than a few minutes.');
  // Signed over the name as sent: a name trimmed here still had to be signed whole.
  const hostId = await verifySigned(String(key), `glass-claim-v1\n${code}\n${ts}\n${name}`, String(sig));
  if (!hostId) throw new Refused('The claim isn\'t signed by that computer.', 401);
  const now = Date.now();
  const visitor = await visitorId(request, env);
  const recent = await env.DB.prepare('SELECT COUNT(*) AS n FROM computer_claims WHERE visitor = ? AND created > ?').bind(visitor, now - 3600_000).first('n');
  if (recent >= CLAIMS_PER_HOUR) throw new Refused('Too many links. Try again later.', 429);
  await env.DB.prepare('DELETE FROM computer_claims WHERE expires < ?').bind(now).run();
  const res = await env.DB.prepare(
    'INSERT OR IGNORE INTO computer_claims (code_hash, host_id, host_key, name, created, expires, visitor) VALUES (?, ?, ?, ?, ?, ?, ?)',
  )
    .bind(await sha256b64(`claim:${code}`), hostId, String(key), clean, now, now + CLAIM_MS, visitor)
    .run();
  if (!res.meta.changes) throw new Refused('That code was used already.', 409);
  return json({ url: `${url.origin}/computers#claim=${code}`, expires: Math.floor((now + CLAIM_MS) / 1000) });
}

/** A host takes itself off every account (and drops its unused claims). */
async function unclaim(request, env) {
  const { key, ts, sig } = await body(request);
  if (Math.abs(Date.now() - Number(ts) * 1000) > CLAIM_CLOCK_MS) throw new Refused('This computer\'s clock is off by more than a few minutes.');
  const hostId = await verifySigned(String(key ?? ''), `glass-unclaim-v1\n${ts}`, String(sig ?? ''));
  if (!hostId) throw new Refused('Not signed by that computer.', 401);
  const [removed] = await env.DB.batch([
    env.DB.prepare('DELETE FROM account_computers WHERE host_id = ?').bind(hostId),
    env.DB.prepare('DELETE FROM computer_claims WHERE host_id = ?').bind(hostId),
    dropOrphanReports(env),
  ]);
  return json({ removed: removed.meta.changes });
}

/** Which computer a claim link adds, so the page can ask first. */
async function peekClaim(env, url) {
  const claim = await env.DB.prepare('SELECT name FROM computer_claims WHERE code_hash = ? AND expires > ?')
    .bind(await sha256b64(`claim:${url.searchParams.get('code') ?? ''}`), Date.now())
    .first();
  if (!claim) throw new Refused('This link has expired or was used already. Make a new one with: glass account add', 404);
  return json({ name: claim.name });
}

async function addComputer(request, env) {
  const s = await session(request, env);
  if (!s) throw new Refused('Sign in first.', 401);
  const { claim: code } = await body(request);
  const n = await env.DB.prepare('SELECT COUNT(*) AS n FROM account_computers WHERE account = ?').bind(s.account).first('n');
  if (n >= COMPUTERS_PER_ACCOUNT) throw new Refused('This account has as many computers as it can hold.');
  // Taken in one step, so two browsers can't both use one link.
  const claim = await env.DB.prepare('DELETE FROM computer_claims WHERE code_hash = ? AND expires > ? RETURNING host_id, host_key, name')
    .bind(await sha256b64(`claim:${code}`), Date.now())
    .first();
  if (!claim) throw new Refused('This link has expired or was used already. Make a new one with: glass account add', 404);
  await env.DB.prepare('INSERT OR REPLACE INTO account_computers (account, host_id, host_key, name, added) VALUES (?, ?, ?, ?, ?)')
    .bind(s.account, claim.host_id, claim.host_key, claim.name, Date.now())
    .run();
  return json({ name: claim.name });
}

async function computers(request, env) {
  const s = await session(request, env);
  if (!s) throw new Refused('Sign in first.', 401);
  const { results } = await env.DB.prepare(
    `SELECT c.host_id, c.host_key, c.name, c.added, r.reported, r.data
       FROM account_computers c LEFT JOIN computer_reports r ON r.host_id = c.host_id
      WHERE c.account = ? ORDER BY c.name COLLATE NOCASE`,
  )
    .bind(s.account)
    .all();
  const online = await hostsOnline(env, results.map((r) => r.host_id));
  return json({
    computers: results.map((r) => ({
      id: r.host_id,
      key: r.host_key,
      name: r.name,
      added: r.added,
      online: online[r.host_id],
      ...(r.data && { report: { ...JSON.parse(r.data), at: r.reported } }),
    })),
  });
}

const POLICY = { filevault: false, firewall: false, glass_current: false, disk_free_gb: 0, fresh_hours: 0, os_min: '' };

/** Only the fields a policy may have, each the right kind and size; the rest is dropped. */
export function cleanPolicy(raw) {
  const out = { ...POLICY };
  for (const k of ['filevault', 'firewall', 'glass_current']) if (raw?.[k] === true) out[k] = true;
  if (Number.isFinite(raw?.disk_free_gb)) out.disk_free_gb = Math.min(100_000, Math.max(0, Math.round(raw.disk_free_gb)));
  if (Number.isFinite(raw?.fresh_hours)) out.fresh_hours = Math.min(24 * 365, Math.max(0, Math.round(raw.fresh_hours)));
  if (typeof raw?.os_min === 'string' && /^\d{1,3}(\.\d{1,3}){0,2}$/.test(raw.os_min.trim())) out.os_min = raw.os_min.trim();
  return out;
}

async function getPolicy(request, env) {
  const s = await session(request, env);
  if (!s) throw new Refused('Sign in first.', 401);
  const row = await env.DB.prepare('SELECT data FROM account_policy WHERE account = ?').bind(s.account).first('data');
  return json({ policy: cleanPolicy(row ? JSON.parse(row) : {}) });
}

async function putPolicy(request, env) {
  const s = await session(request, env);
  if (!s) throw new Refused('Sign in first.', 401);
  const policy = cleanPolicy(await body(request));
  await env.DB.prepare('INSERT OR REPLACE INTO account_policy (account, data) VALUES (?, ?)').bind(s.account, JSON.stringify(policy)).run();
  return json({ policy });
}

const RELAY_MAX = 4096;

/**
 * An action (signed by a passkey) or an enrollment for one of this account's computers, handed
 * to the computer through the introducer. The computer checks everything itself; this only
 * makes sure the account owns the computer and the request is a sane size.
 */
async function relayToComputer(request, env, hostId, what) {
  const s = await session(request, env);
  if (!s) throw new Refused('Sign in first.', 401);
  const mine = await env.DB.prepare('SELECT 1 AS x FROM account_computers WHERE account = ? AND host_id = ?').bind(s.account, hostId).first();
  if (!mine) throw new Refused('That computer isn\'t on your account.', 404);
  const text = await request.text();
  if (text.length > RELAY_MAX) throw new Refused('too large', 413);
  let b;
  try {
    b = JSON.parse(text);
  } catch {
    throw new Refused('expected JSON');
  }
  const str = (v, max) => typeof v === 'string' && v.length > 0 && v.length <= max;
  let payload;
  if (what === 'action') {
    const a = b?.assertion;
    if (!str(b?.cmd, 1024) || !a || !['id', 'clientDataJSON', 'authenticatorData', 'signature'].every((k) => str(a[k], 1500))) throw new Refused('expected cmd and a passkey assertion');
    payload = { kind: 'command', cmd: b.cmd, assertion: { id: a.id, clientDataJSON: a.clientDataJSON, authenticatorData: a.authenticatorData, signature: a.signature } };
  } else {
    if (!str(b?.cred, 1024) || !str(b?.key, 200) || !str(b?.mac, 100)) throw new Refused('expected cred, key and mac');
    payload = { kind: 'enroll', cred: b.cred, key: b.key, mac: b.mac };
  }
  const res = await env.INTRODUCER.get(env.INTRODUCER.idFromName(hostId)).fetch(`https://introducer/?role=relay&id=${hostId}`, {
    method: 'POST',
    body: JSON.stringify(payload),
  });
  const out = await res.json();
  return res.status === 409 || res.status === 429 ? json({ error: out.message }, res.status) : json(out);
}

/** Drops reports of computers that are on no account any more (statement for a batch). */
function dropOrphanReports(env) {
  return env.DB.prepare('DELETE FROM computer_reports WHERE host_id NOT IN (SELECT host_id FROM account_computers)');
}

const REPORT_MAX = 4096;
const REPORT_EVERY_MS = 5_000;
const REPORT_STRINGS = ['os', 'model', 'chip', 'version'];
const REPORT_NUMBERS = ['ram_gb', 'disk_total_gb', 'disk_free_gb', 'uptime_h', 'battery'];
const REPORT_FLAGS = ['filevault', 'firewall', 'managed'];

/** Only the fields a report may have, each the right kind and size; anything else is dropped. */
export function cleanReport(raw) {
  const out = {};
  for (const k of REPORT_STRINGS) if (typeof raw[k] === 'string') out[k] = raw[k].replace(/[\u0000-\u001f\u007f]/g, '').slice(0, 80);
  for (const k of REPORT_NUMBERS) if (Number.isFinite(raw[k]) && raw[k] >= 0 && raw[k] < 1e7) out[k] = Math.round(raw[k]);
  for (const k of REPORT_FLAGS) if (typeof raw[k] === 'boolean') out[k] = raw[k];
  return out;
}

/** A computer's report, signed with its key. Empty data deletes it. Only computers on an account. */
async function fleetReport(request, env) {
  const { key, ts, data, sig } = await body(request);
  if (typeof data !== 'string' || data.length > REPORT_MAX) throw new Refused('expected key, ts, data and sig');
  if (Math.abs(Date.now() - Number(ts) * 1000) > CLAIM_CLOCK_MS) throw new Refused('This computer\'s clock is off by more than a few minutes.');
  const hostId = await verifySigned(String(key ?? ''), `glass-fleet-v1\n${ts}\n${data}`, String(sig ?? ''));
  if (!hostId) throw new Refused('Not signed by that computer.', 401);
  if (data === '') {
    await env.DB.prepare('DELETE FROM computer_reports WHERE host_id = ?').bind(hostId).run();
    return json({ deleted: true });
  }
  const onAccount = await env.DB.prepare('SELECT 1 AS x FROM account_computers WHERE host_id = ? LIMIT 1').bind(hostId).first();
  if (!onAccount) throw new Refused('This computer isn\'t on an account. Add it with: glass account add', 403);
  let raw;
  try {
    raw = JSON.parse(data);
  } catch {
    throw new Refused('data isn\'t JSON');
  }
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw new Refused('data isn\'t an object');
  const now = Date.now();
  const last = await env.DB.prepare('SELECT reported FROM computer_reports WHERE host_id = ?').bind(hostId).first('reported');
  if (last && now - last < REPORT_EVERY_MS) throw new Refused('Reports come at most every few seconds.', 429);
  await env.DB.prepare('INSERT OR REPLACE INTO computer_reports (host_id, reported, data) VALUES (?, ?, ?)')
    .bind(hostId, now, JSON.stringify(cleanReport(raw)))
    .run();
  return json({ ok: true });
}

async function removeComputer(request, env, hostId) {
  const s = await session(request, env);
  if (!s) throw new Refused('Sign in first.', 401);
  await env.DB.batch([
    env.DB.prepare('DELETE FROM account_computers WHERE account = ? AND host_id = ?').bind(s.account, hostId),
    dropOrphanReports(env),
  ]);
  return json({ ok: true });
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
