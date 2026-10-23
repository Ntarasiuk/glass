// Links made with `glass link --passkey`: the device that opens one registers a passkey,
// and its guest pass only works together with that passkey from then on.
// Chrome's virtual authenticator (over CDP) stands in for Touch ID or a security key.

import { Host, expectVideo, hudText, test, expect } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

async function addAuthenticator(page) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('WebAuthn.enable');
  const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true },
  });
  return { cdp, authenticatorId };
}

async function passkeyLink(host) {
  await new Promise((r) => setTimeout(r, 1500)); // the host has written its status, which `glass link` reads
  const made = JSON.parse(host.cli('link', '--json', '--for', '1h', '--passkey'));
  expect(made.scope.passkey).toBe(true);
  return `${host.base}/#l=${made.url.match(/#l=([0-9a-f]{32})$/)[1]}`;
}

it('a passkey link registers a passkey, then needs it to sign in again', async ({ browser, host }) => {
  const link = await passkeyLink(host);
  const context = await browser.newContext();
  const page = await context.newPage();
  const { cdp, authenticatorId } = await addAuthenticator(page);

  await page.goto(link);
  await expectVideo(page);
  const { credentials } = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
  expect(credentials).toHaveLength(1);
  expect(host.cli('link', '--list')).toContain('passkey');

  // A reload signs in with the saved pass, which asks the passkey to sign a fresh challenge.
  await page.reload();
  await expectVideo(page);
  const after = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
  expect(after.credentials[0].signCount).toBeGreaterThan(credentials[0].signCount);

  // The same pass in another browser, without the passkey, gets nowhere.
  const pass = await page.evaluate(() => localStorage.getItem('glass.pass'));
  expect(pass).toBeTruthy();
  const thiefContext = await browser.newContext();
  const thief = await thiefContext.newPage();
  // Without that passkey the browser's sheet has nothing to offer. Answer as it would,
  // in the page: a real lookup falls through to the system's own passkeys (on a Mac, a
  // Keychain access prompt for whatever app launched the tests).
  await thief.addInitScript(() => {
    navigator.credentials.get = () => Promise.reject(new DOMException('no such passkey', 'NotAllowedError'));
  });
  await thief.goto(`${host.base}/config.json`);
  await thief.evaluate((p) => localStorage.setItem('glass.pass', p), pass);
  await thief.goto(`${host.base}/`);
  await expect(thief.locator('.toast', { hasText: 'needs a passkey' }).first()).toBeVisible({ timeout: 15_000 });
  expect((await hudText(thief)).includes('640×360')).toBe(false);

  // The owner's device is still fine.
  await expectVideo(page);
  await thiefContext.close();
  await context.close();
});

it('a passkey link without a passkey is refused', async ({ browser, host }) => {
  const link = await passkeyLink(host);
  const context = await browser.newContext();
  const page = await context.newPage();
  // The person cancels the passkey sheet, so the host never lets it in.
  await page.addInitScript(() => {
    navigator.credentials.create = () => Promise.reject(new DOMException('cancelled', 'NotAllowedError'));
  });
  await page.goto(link);
  await expect(page.locator('.toast', { hasText: 'needs a passkey' }).first()).toBeVisible({ timeout: 15_000 });
  expect((await hudText(page)).includes('640×360')).toBe(false);
  await context.close();
});
