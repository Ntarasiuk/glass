// Single sign-on (OpenID Connect) against a fake provider run by the test
// (oidc-provider.mjs).

import { test, expect } from '@playwright/test';
import { Host, expectVideo } from './harness.mjs';
import { fakeProvider } from './oidc-provider.mjs';

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
