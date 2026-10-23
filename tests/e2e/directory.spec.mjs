// The directory: access rules (glass acl and its API), SCIM provisioning feeding them,
// deprovisioning cutting sessions off, and LDAP sign-in (against a real OpenLDAP in
// Docker, skipped without Docker).

import { execFileSync } from 'node:child_process';
import { Host, expectVideo, freePort, test, expect } from './harness.mjs';
import { fakeProvider, CLIENT_SECRET } from './oidc-provider.mjs';

const SCIM_TOKEN = 'scim-test-token-0123456789';

function scim(host, method, path, body, token = SCIM_TOKEN) {
  return fetch(`${host.base}/scim/v2${path}`, {
    method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/scim+json' },
    body: body && JSON.stringify(body),
  });
}

async function directoryHost(provider) {
  const host = new Host();
  host.cli('config', 'oidc-issuer', provider.issuer);
  host.cli('config', 'oidc-client-id', 'glass-test');
  host.cli('config', 'oidc-client-secret', CLIENT_SECRET);
  host.cli('config', 'scim-token', SCIM_TOKEN);
  return host.start();
}

test('SCIM groups pick the access rule, and deprovisioning cuts the session off', async ({ browser }) => {
  const provider = await fakeProvider({ email: 'ana@example.com' });
  const host = await directoryHost(provider);
  try {
    // Engineers watch only; everyone else at example.com gets full control.
    host.cli('acl', 'add', 'group:eng', '--view-only', '--for', '8h');
    host.cli('acl', 'add', '@example.com');
    expect(host.cli('acl')).toMatch(/group:eng\s+view only\s+for 8 hours/);

    expect((await scim(host, 'GET', '/Users', undefined, 'wrong')).status).toBe(401);
    expect((await scim(host, 'GET', '/ServiceProviderConfig')).status).toBe(200);
    const user = await (await scim(host, 'POST', '/Users', { schemas: ['urn:ietf:params:scim:schemas:core:2.0:User'], userName: 'ana@example.com', active: true })).json();
    expect(user.id).toBeTruthy();
    expect((await scim(host, 'POST', '/Users', { userName: 'ANA@example.com' })).status).toBe(409);
    const group = await (await scim(host, 'POST', '/Groups', { displayName: 'eng', members: [{ value: user.id }] })).json();
    expect(group.members).toEqual([{ value: user.id, display: 'ana@example.com' }]);
    const found = await (await scim(host, 'GET', `/Users?filter=${encodeURIComponent('userName eq "ana@example.com"')}`)).json();
    expect(found.totalResults).toBe(1);

    const page = await (await browser.newContext()).newPage();
    await page.goto(host.url({ token: false }));
    await page.locator('#sso').click();
    await expectVideo(page);
    await expect(page.locator('#btn-keyboard')).toBeHidden(); // view only, from group:eng
    const { passes } = JSON.parse(host.cli('link', '--list', '--json'));
    expect(passes).toHaveLength(1);
    expect(passes[0].scope.view_only).toBe(true);

    // Out of the group: the next sign-in falls through to @example.com (full control).
    await scim(host, 'PATCH', `/Groups/${group.id}`, { Operations: [{ op: 'remove', path: `members[value eq "${user.id}"]` }] });

    // The provider deactivates Ana: her pass goes now, the session within seconds.
    const patched = await scim(host, 'PATCH', `/Users/${user.id}`, { Operations: [{ op: 'replace', path: 'active', value: false }] });
    expect((await patched.json()).active).toBe(false);
    expect(JSON.parse(host.cli('link', '--list', '--json')).passes).toEqual([]);
    await expect(page.locator('#auth')).toBeVisible({ timeout: 25_000 });
    const again = await (await browser.newContext()).newPage();
    await again.goto(`${host.base}/sso/start`);
    await expect(again.locator('body')).toContainText("isn't allowed");

    // Reactivated (Entra ID style: no path, "True" as a string): full control now.
    await scim(host, 'PATCH', `/Users/${user.id}`, { Operations: [{ op: 'Replace', value: { active: 'True' } }] });
    const back = await (await browser.newContext()).newPage();
    await back.goto(host.url({ token: false }));
    await back.locator('#sso').click();
    await expectVideo(back);
    await expect(back.locator('#btn-keyboard')).toBeVisible();

    expect((await scim(host, 'DELETE', `/Users/${user.id}`)).status).toBe(204);
    await expect(back.locator('#auth')).toBeVisible({ timeout: 25_000 });
  } finally {
    await host.stop();
    await provider.close();
  }
});

test('the access rules API is for the owner only', async () => {
  const host = await new Host().start();
  try {
    const url = `${host.base}/admin/api/acl`;
    expect((await fetch(url)).status).toBe(403);
    const auth = { authorization: `Bearer ${host.token}`, 'content-type': 'application/json' };
    expect(await (await fetch(url, { headers: auth })).json()).toEqual({ rules: [], implicit: true });
    const bad = await fetch(url, { method: 'PUT', headers: auth, body: JSON.stringify({ rules: [{ match: 'not a rule' }] }) });
    expect(bad.status).toBe(400);
    const rules = [{ match: '@corp.example', scope: { view_only: true, files: false, clipboard: true, ask: false, passkey: false }, duration_secs: 3600 }];
    expect((await fetch(url, { method: 'PUT', headers: auth, body: JSON.stringify({ rules }) })).status).toBe(200);
    expect(JSON.parse(host.cli('acl', '--json')).rules).toEqual(rules);
    expect((await fetch(url, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: '{"rules":[]}' })).status).toBe(403);
    host.cli('acl', 'remove', '0');
    expect(JSON.parse(host.cli('acl', '--json')).rules).toEqual([]);
  } finally {
    await host.stop();
  }
});

const dockerUp = (() => {
  try {
    execFileSync('docker', ['info'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
})();

const SEED = `dn: ou=people,dc=example,dc=com
objectClass: organizationalUnit
ou: people

dn: ou=groups,dc=example,dc=com
objectClass: organizationalUnit
ou: groups

dn: uid=ana,ou=people,dc=example,dc=com
objectClass: inetOrgPerson
uid: ana
cn: Ana
sn: A
mail: ana@example.com
userPassword: secret

dn: cn=eng,ou=groups,dc=example,dc=com
objectClass: groupOfUniqueNames
cn: eng
uniqueMember: uid=ana,ou=people,dc=example,dc=com
`;

/** An OpenLDAP server with Ana (password "secret") in group eng; admin/admin. */
async function openldap() {
  const port = await freePort('tcp');
  const name = `glass-e2e-ldap-${port}`;
  execFileSync('docker', ['run', '-d', '--rm', '--name', name, '-p', `127.0.0.1:${port}:389`,
    '-e', 'LDAP_ORGANISATION=Example', '-e', 'LDAP_DOMAIN=example.com', '-e', 'LDAP_ADMIN_PASSWORD=admin', '-e', 'LDAP_TLS=false',
    'osixia/openldap:1.5.0'], { stdio: 'ignore' });
  const admin = ['-x', '-H', 'ldap://localhost', '-D', 'cn=admin,dc=example,dc=com', '-w', 'admin'];
  for (let i = 0; ; i++) {
    try {
      execFileSync('docker', ['exec', name, 'ldapsearch', ...admin, '-b', 'dc=example,dc=com', '-s', 'base'], { stdio: 'ignore' });
      break;
    } catch {
      if (i > 90) throw new Error('OpenLDAP never started');
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  const stop = () => execFileSync('docker', ['rm', '-f', name], { stdio: 'ignore' });
  // The image restarts slapd once after its first-boot setup, so the first search can
  // succeed and the seed still land on a server going down: retry, and never leak it.
  for (let i = 0; ; i++) {
    try {
      execFileSync('docker', ['exec', '-i', name, 'ldapadd', '-c', ...admin], { input: SEED, stdio: ['pipe', 'ignore', 'ignore'] });
      break;
    } catch (e) {
      if (e.status === 68) break; // already there: an earlier try got through
      if (i > 20) {
        stop();
        throw e;
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  return { url: `ldap://localhost:${port}`, stop };
}

test('signs in with a directory (LDAP) account', async ({ browser }) => {
  test.skip(!dockerUp, 'needs Docker for an OpenLDAP server');
  test.setTimeout(180_000);
  const ldap = await openldap();
  const host = new Host();
  try {
    host.cli('config', 'ldap-url', ldap.url);
    host.cli('config', 'ldap-user-dn', 'uid={user},ou=people,dc=example,dc=com');
    host.cli('acl', 'add', 'group:eng', '--view-only');
    await host.start();
    const page = await (await browser.newContext()).newPage();
    await page.goto(host.url({ token: false }));
    await expect(page.locator('#ldap')).toBeVisible();
    await page.locator('#ldap input[name="ldap-user"]').fill('ana');
    await page.locator('#ldap input[name="ldap-password"]').fill('wrong');
    await page.locator('#ldap-submit').click();
    await expect(page.locator('#ldap-error')).toHaveText("That username or password didn't work.");
    await page.locator('#ldap input[name="ldap-password"]').fill('secret');
    await page.locator('#ldap input[name="ldap-password"]').press('Enter');
    await expectVideo(page);
    await expect(page.locator('#btn-keyboard')).toBeHidden(); // group eng, via memberOf
    const [pass] = JSON.parse(host.cli('link', '--list', '--json')).passes;
    expect(pass.device).toBe('ana@example.com');

    // Search-then-bind, as the admin account; an injection attempt finds nobody.
    host.cli('config', 'ldap-user-dn', '');
    host.cli('config', 'ldap-base-dn', 'dc=example,dc=com');
    host.cli('config', 'ldap-bind-dn', 'cn=admin,dc=example,dc=com');
    host.cli('config', 'ldap-bind-password', 'admin');
    const post = (user, password) => fetch(`${host.base}/sso/ldap`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ user, password }) });
    const ok = await post('ana@example.com', 'secret');
    expect(ok.status).toBe(200);
    expect((await ok.json()).pass).toMatch(/^[0-9a-f]{32}$/);
    expect((await post('*', 'secret')).status).toBe(403);
    expect((await post('ana', '')).status).toBe(403); // an empty password never binds
    // No rule matches someone outside eng.
    host.cli('acl', 'remove', '0');
    host.cli('acl', 'add', 'group:admins');
    expect((await post('ana', 'secret')).status).toBe(403);
  } finally {
    await host.stop();
    ldap.stop();
  }
});
