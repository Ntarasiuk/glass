// Remote access over WebRTC, end to end: the site's introducer (wrangler dev, locally), a
// test host with remote access on, and a browser opening a `glass link --anywhere` link
// from the site's /app/ like it would from glassdesk.dev.

import { test, expect } from '@playwright/test';
import { spawn, execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { Host, expectVideo, freePort } from './harness.mjs';

const SITE = resolve(import.meta.dirname, '../../site');

test.describe.configure({ mode: 'serial' });

let introducer;
let base;

test.beforeAll(async () => {
  execFileSync('node', ['build.mjs'], { cwd: SITE, stdio: 'ignore' });
  const port = await freePort('tcp');
  base = `http://localhost:${port}`;
  introducer = spawn('npx', ['wrangler', 'dev', '--local', '--port', String(port), '--inspector-port', String(await freePort('tcp'))], {
    cwd: SITE,
    stdio: 'ignore',
    detached: true,
  });
  for (let i = 0; i < 300; i++) {
    try {
      if ((await fetch(`${base}/app/`)).ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error('wrangler dev did not start');
});

test.afterAll(() => {
  // wrangler starts workerd as a child: end the whole group.
  try { process.kill(-introducer.pid, 'SIGTERM'); } catch {}
});

async function remoteHost() {
  const host = await new Host({ env: { GLASS_BASE: base } }).start();
  // `glass link` needs the running host's status file.
  await expect.poll(() => { try { return host.cli('status', '--json').includes('"running":true'); } catch { return false; } }).toBe(true);
  return host;
}

test('a link from anywhere connects over WebRTC: video, input and files', async ({ page }) => {
  const host = await remoteHost();
  try {
    const { url } = JSON.parse(host.cli('link', '--anywhere', '--json'));
    expect(url).toMatch(new RegExp(`^${base}/app/#r=[A-Za-z0-9_-]+&l=[0-9a-f]{32}$`));
    await page.goto(url);
    await expectVideo(page);
    await expect(page.locator('.toast', { hasText: "You're in" })).toBeVisible();
    // The link is spent and dropped from the URL; the host key stays (it names the computer).
    expect(page.url()).toMatch(/#r=[A-Za-z0-9_-]+$/);
    await expect(page.locator('#btn-machines')).toBeHidden();

    // Input reaches the host.
    await host.clearInput();
    await page.keyboard.press('a');
    await expect.poll(async () => (await host.input('keydown')).events.length).toBe(1);

    // Files both ways (streams over their own data channels, with backpressure).
    const body = Buffer.alloc(2 * 1024 * 1024, 7);
    await page.locator('#file-input').setInputFiles([{ name: 'remote.bin', mimeType: 'application/octet-stream', buffer: body }]);
    const files = async () => Object.fromEntries((await host.get('/test/downloads')).files.map((f) => [f.name, f.size]));
    await expect.poll(files, { timeout: 20_000 }).toEqual({ 'remote.bin': body.length });
    await page.mouse.move(640, 8); // bring back the tucked toolbar, as a mouse heading up does
    await page.locator('#btn-download').click();
    await expect(page.locator('.toast', { hasText: 'glass-test-pick', has: page.locator('.action') })).toBeVisible({ timeout: 10_000 });

    // Its logs reach the host over the connection (there's no web server to post them to).
    await expect.poll(() => host.clientLog('remote: connected').length, { timeout: 10_000 }).toBeGreaterThan(0);

    // A reload signs in with the saved pass, not the spent link.
    await page.reload();
    await expectVideo(page);
  } finally {
    await host.stop();
  }
});

test('says so when the computer is offline or remote access is off', async ({ page }) => {
  const host = await remoteHost();
  try {
    const { url } = JSON.parse(host.cli('link', '--anywhere', '--json'));
    host.cli('remote', 'off');
    await page.waitForTimeout(4000); // the host notices within ~3 s and disconnects
    await page.goto(url);
    await expect(page.locator('#status')).toContainText(/offline, or remote access is turned off/, { timeout: 20_000 });
  } finally {
    await host.stop();
  }
});
