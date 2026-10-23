// A fake OpenID Connect provider for tests: discovery, an authorize endpoint that signs
// `provider.email` straight in, a token endpoint issuing ES256 ID tokens, and its key set.

import { createServer } from 'node:http';
import { generateKeyPairSync, sign, createHash, randomBytes } from 'node:crypto';
import { freePort } from './harness.mjs';

const b64url = (buf) => Buffer.from(buf).toString('base64url');

/** Client secret the provider expects. */
export const CLIENT_SECRET = 'shh';

export async function fakeProvider({ email }) {
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
      if (!flow || !pkceOk || form.get('client_secret') !== CLIENT_SECRET || form.get('redirect_uri') !== flow.redirect) {
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
