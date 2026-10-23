// The host's shell in the viewer, and forwarded ports (/fwd/<port>/).

import { test, expect } from '@playwright/test';
import { createServer } from 'node:http';
import { Host, expectVideo, freePort } from './harness.mjs';

/** A plain /bin/sh, so the test doesn't depend on the user's shell setup. */
const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host({ env: { SHELL: '/bin/sh' } }).start();
    await use(host);
    await host.stop();
  },
});

/** A local web app on a free port: echoes its path, and redirects /go to /login. */
async function app() {
  const port = await freePort('tcp');
  const server = createServer((req, res) => {
    if (req.url === '/go') {
      res.writeHead(302, { location: '/login' });
      return res.end();
    }
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end(`path=${req.url} cookie=${req.headers.cookie ?? ''}`);
  });
  await new Promise((r) => server.listen(port, '127.0.0.1', r));
  return { port, close: () => new Promise((r) => server.close(r)) };
}

test.describe('terminal', () => {
  it('opens a shell on the host and runs a command', async ({ page, host }) => {
    await page.goto(host.url());
    await expectVideo(page);
    await page.locator('#btn-terminal').click();
    await expect(page.locator('#terminal')).toBeVisible();
    await expect(page.locator('#terminal .xterm')).toBeVisible();
    await page.locator('#terminal .xterm-helper-textarea').focus();
    await page.keyboard.type('echo glass-$((40+2))');
    await page.keyboard.press('Enter');
    await expect(page.locator('#terminal .xterm-rows')).toContainText('glass-42', { timeout: 10_000 });
    // Typing in the terminal never reaches the remote desktop.
    expect((await host.input('keydown')).events).toHaveLength(0);
    // Hide keeps the shell; End shell stops it.
    await page.locator('#terminal-close').click();
    await expect(page.locator('#terminal')).toBeHidden();
  });

  it('is not offered to guests', async ({ page, host }) => {
    const { url } = JSON.parse(host.cli('link', '--json'));
    await page.goto(url.replace(/^https?:\/\/[^/]+/, host.base));
    await expectVideo(page);
    await expect(page.locator('#btn-terminal')).toBeHidden();
  });
});

test.describe('port forwarding', () => {
  it('lets the owner open a listed port and nobody else', async ({ page, host }) => {
    const web = await app();
    const other = await app();
    try {
      host.cli('config', 'forward-ports', String(web.port));
      const fwd = `${host.base}/fwd/${web.port}`;

      // No cookie, a wrong token: refused.
      expect((await fetch(`${fwd}/`)).status).toBe(403);
      expect((await fetch(`${host.base}/fwd/auth?t=wrong`, { redirect: 'manual' })).status).toBe(403);

      // The token buys a cookie, then the app answers through the prefix.
      const auth = await fetch(`${host.base}/fwd/auth?t=${host.token}&to=/fwd/${web.port}/x`, { redirect: 'manual' });
      expect(auth.status).toBe(303);
      expect(auth.headers.get('location')).toBe(`/fwd/${web.port}/x`);
      const cookie = auth.headers.get('set-cookie').split(';')[0];
      expect(cookie).toMatch(/^glass_fwd=[0-9a-f]{64}$/);

      const r = await fetch(`${fwd}/x?q=1`, { headers: { cookie: `${cookie}; app=1` } });
      expect(r.status).toBe(200);
      // Our cookie stays with Glass; the app's own cookies pass through.
      expect(await r.text()).toBe('path=/x?q=1 cookie=app=1');

      // Redirects stay under the prefix.
      const go = await fetch(`${fwd}/go`, { headers: { cookie }, redirect: 'manual' });
      expect(go.status).toBe(302);
      expect(go.headers.get('location')).toBe(`/fwd/${web.port}/login`);

      // A port that isn't listed, even with the cookie.
      expect((await fetch(`${host.base}/fwd/${other.port}/`, { headers: { cookie } })).status).toBe(404);

      // The viewer offers the port and opens it through /fwd/auth.
      await page.goto(host.url());
      await expectVideo(page);
      await page.locator('#btn-ports').click();
      const [tab] = await Promise.all([
        page.context().waitForEvent('page'),
        page.locator('#ports-panel button', { hasText: `localhost:${web.port}` }).click(),
      ]);
      await expect(tab.locator('body')).toContainText('path=/ ');
    } finally {
      await web.close();
      await other.close();
    }
  });

  it('shows no ports when none are forwarded', async ({ page, host }) => {
    await page.goto(host.url());
    await expectVideo(page);
    await expect(page.locator('#btn-ports')).toBeHidden();
  });
});
