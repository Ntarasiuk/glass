// Single sign-on (OpenID Connect) against a fake provider run by the test: discovery,
// an authorize endpoint that signs the user straight in, a token endpoint issuing ES256
// ID tokens, and its key set.

import { test, expect } from '@playwright/test';
import { createServer } from 'node:http';
import { generateKeyPairSync, sign, createHash, randomBytes } from 'node:crypto';
import { Host, expectVideo, freePort } from './harness.mjs';

const b64url = (buf) => Buffer.from(buf).toString('base64url');

async function fakeProvider({ email }) {
  const port = await freePort('tcp');
  const issuer = `http://localhost:${port}`;
  const { privateKey, publicKey } = generateKeyPairSync('ec', { namedCurve: 'P-256' });
  const jwk = { ...publicKey.export({ format: 'jwk' }), kid: 'test', alg: 'ES256', use: 'sig' };
  const codes = new Map(); // code -> what /authorize was asked for
  const provider = { issuer, email, tokenRequests: [] };
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, issuer);
    const json = (body) => {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify(body));
    };
    if (url.pathname === '/.well-known/openid-configuration') {
      return json({ issuer, authorization_endpoint: `${issuer}/authorize`, token_endpoint: `${issuer}/token`, jwks_uri: `${issuer}/jwks` });
    }
    if (url.pathname === '/jwks') return json({ keys: [jwk] });
    if (url.pathname === '/authorize') {
      const p = url.searchParams;
      const code = b64url(randomBytes(16));
      codes.set(code, { nonce: p.get('nonce'), challenge: p.get('code_challenge'), redirect: p.get('redirect_uri'), client: p.get('client_id') });
      res.writeHead(302, { location: `${p.get('redirect_uri')}?code=${code}&state=${p.get('state')}` });
      return res.end();
    }
    if (url.pathname === '/token' && req.method === 'POST') {
      let body = '';
      for await (const chunk of req) body += chunk;
      const form = new URLSearchParams(body);
      provider.tokenRequests.push(Object.fromEntries(form));
      const flow = codes.get(form.get('code'));
      codes.delete(form.get('code'));
      const pkceOk = flow && b64url(createHash('sha256').update(form.get('code_verifier') ?? '').digest()) === flow.challenge;
      if (!flow || !pkceOk || form.get('client_secret') !== 'shh' || form.get('redirect_uri') !== flow.redirect) {
        res.statusCode = 400;
        return json({ error: 'invalid_grant' });
      }
      const header = b64url(JSON.stringify({ alg: 'ES256', kid: 'test', typ: 'JWT' }));
      const now = Math.floor(Date.now() / 1000);
      const claims = b64url(JSON.stringify({ iss: issuer, aud: flow.client, exp: now + 300, iat: now, nonce: flow.nonce, email: provider.email, email_verified: true, sub: '1' }));
      const sig = sign('sha256', Buffer.from(`${header}.${claims}`), { key: privateKey, dsaEncoding: 'ieee-p1363' });
      return json({ id_token: `${header}.${claims}.${b64url(sig)}`, access_token: 'x', token_type: 'Bearer' });
    }
    res.statusCode = 404;
    res.end();
  });
  await new Promise((r) => server.listen(port, 'localhost', r));
  provider.close = () => new Promise((r) => server.close(r));
  return provider;
}

async function ssoHost(provider) {
  const host = new Host();
  host.cli('config', 'oidc-issuer', provider.issuer);
  host.cli('config', 'oidc-client-id', 'glass-test');
  host.cli('config', 'oidc-client-secret', 'shh');
  host.cli('config', 'oidc-allow', 'ana@example.com,@corp.example');
  return host.start();
}

test('signs in with SSO and gets a pass that survives a reload', async ({ browser }) => {
  const provider = await fakeProvider({ email: 'bob@corp.example' });
  const host = await ssoHost(provider);
  try {
    expect(host.cli('config', 'oidc-client-secret').trim()).toBe('(set)');
    const page = await (await browser.newContext()).newPage();
    await page.goto(host.url({ token: false }));
    await expect(page.locator('#auth')).toBeVisible();
    await page.locator('#sso').click();
    await expectVideo(page);
    expect(page.url()).not.toContain('#p=');
    expect(provider.tokenRequests[0]).toMatchObject({ grant_type: 'authorization_code', client_id: 'glass-test' });
    await page.reload();
    await expectVideo(page);
    const { passes } = JSON.parse(host.cli('link', '--list', '--json'));
    expect(passes.map((p) => p.device)).toEqual(['bob@corp.example']);
    const log = JSON.parse(host.cli('log', '--json')).connections;
    expect(log.some((e) => e.how === 'sso' && e.detail === 'sso bob@corp.example')).toBe(true);
  } finally {
    await host.stop();
    await provider.close();
  }
});

test('refuses an account that is not on the allow-list', async ({ browser }) => {
  const provider = await fakeProvider({ email: 'mallory@example.com' });
  const host = await ssoHost(provider);
  try {
    const page = await (await browser.newContext()).newPage();
    await page.goto(`${host.base}/sso/start`);
    await expect(page.locator('body')).toContainText("isn't allowed");
    expect(JSON.parse(host.cli('link', '--list', '--json')).passes).toEqual([]);
    // A callback without the flow cookie (a forged or replayed redirect) gets nowhere.
    const r = await fetch(`${host.base}/sso/callback?code=abc&state=def`);
    expect(r.status).toBe(403);
  } finally {
    await host.stop();
    await provider.close();
  }
});

test('the sign-in screen offers no SSO when it is not set up', async ({ page }) => {
  const host = await new Host().start();
  try {
    await page.goto(host.url({ token: false }));
    await expect(page.locator('#auth')).toBeVisible();
    await expect(page.locator('#sso')).toBeHidden();
    expect((await fetch(`${host.base}/sso/start`)).status).toBe(404);
  } finally {
    await host.stop();
  }
});
