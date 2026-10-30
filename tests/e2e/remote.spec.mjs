// Remote access over WebRTC, end to end: the site's introducer (wrangler dev, locally), a
// test host with remote access on, and a browser opening a `glass link --anywhere` link
// from the site's /app/ like it would from glassdesk.dev.

import { spawn, execFileSync } from 'node:child_process';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { BINARY, Host, expectVideo, freePort, test, expect, toolbarClick } from './harness.mjs';

const SITE = resolve(import.meta.dirname, '../../site');

test.describe.configure({ mode: 'serial' });

let introducer;
let base;

test.beforeAll(async () => {
  execFileSync('node', ['build.mjs'], { cwd: SITE, stdio: 'ignore' });
  // Its own database (quick-support codes live in D1).
  const persist = mkdtempSync(join(tmpdir(), 'glass-site-'));
  execFileSync('npx', ['wrangler', 'd1', 'migrations', 'apply', 'getglass', '--local', '--persist-to', persist], { cwd: SITE, stdio: 'ignore' });
  const port = await freePort('tcp');
  base = `http://localhost:${port}`;
  // --local-upstream: the Worker sees this address (not the route's), as passkeys need.
  introducer = spawn('npx', ['wrangler', 'dev', '--local', '--persist-to', persist, '--port', String(port), '--local-upstream', `localhost:${port}`, '--inspector-port', String(await freePort('tcp'))], {
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
  const host = await new Host({ env: { GLASS_BASE: base, GLASS_FLEET_FAKE: '1' } }).start();
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
    // Video comes as a WebRTC track (RTP), shown in a <video> element, not the data channel.
    await expect(page.locator('#screens > video:not(.hidden)')).toHaveCount(1);
    await expect(page.locator('#hud')).toContainText('WebRTC');

    // Input reaches the host.
    await host.clearInput();
    await page.keyboard.press('a');
    await expect.poll(async () => (await host.input('keydown')).events.length).toBe(1);

    // Files both ways (streams over their own data channels, with backpressure).
    const body = Buffer.alloc(2 * 1024 * 1024, 7);
    await page.locator('#file-input').setInputFiles([{ name: 'remote.bin', mimeType: 'application/octet-stream', buffer: body }]);
    const files = async () => Object.fromEntries((await host.get('/test/downloads')).files.map((f) => [f.name, f.size]));
    await expect.poll(files, { timeout: 20_000 }).toEqual({ 'remote.bin': body.length });
    await toolbarClick(page, 'btn-download');
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

test('falls back to video over the data channel when tracks are unavailable', async ({ page }) => {
  const host = await remoteHost();
  try {
    // As if the browser couldn't receive H.264 tracks.
    await page.addInitScript(() => sessionStorage.setItem('glass.rtp', '0'));
    const { url } = JSON.parse(host.cli('link', '--anywhere', '--json'));
    await page.goto(url);
    await expectVideo(page);
    await expect(page.locator('#screens > video:not(.hidden)')).toHaveCount(0);
    await expect(page.locator('#screens > canvas:not(.hidden)')).toHaveCount(1);
  } finally {
    await host.stop();
  }
});

test('quick support: a code typed at /help connects, once allowed, and stopping leaves nothing', async ({ page }) => {
  // `glass support` in test mode: its own temporary config, a test-pattern host, a code.
  const proc = spawn(BINARY, ['support', '--test', '--json'], { env: { ...process.env, GLASS_BASE: base }, stdio: ['ignore', 'pipe', 'inherit'] });
  try {
    const first = await new Promise((resolve, reject) => {
      let out = '';
      proc.stdout.on('data', (d) => {
        out += d;
        if (out.includes('\n')) resolve(JSON.parse(out.split('\n')[0]));
      });
      proc.once('exit', (code) => reject(new Error(`glass support exited (${code})`)));
    });
    expect(first.code).toMatch(/^\d{3} \d{3} \d{3}$/);
    expect(existsSync(first.dir)).toBe(true);
    const hostApi = `http://localhost:${first.http_port}`;

    await page.goto(`${base}/help`);
    await page.locator('#code').fill(first.code.replace(/ /g, ''));
    await page.locator('#code-form button').click();
    await page.waitForURL(/\/app\/#r=/);
    // Asked first: nothing until the person allows it.
    await expect.poll(async () => (await (await fetch(`${hostApi}/test/approvals`)).json()).pending.length, { timeout: 20_000 }).toBe(1);
    await fetch(`${hostApi}/test/approvals?allow=true`, { method: 'POST' });
    await expectVideo(page);
    await expect(page.locator('.toast', { hasText: "You're helping" })).toBeVisible();

    // The code was used up.
    const again = await fetch(`${base}/api/support/claim`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ code: first.code }) });
    expect(again.status).toBe(404);

    // Stopping takes the session's files (token, keys, settings, logs) with it.
    proc.kill('SIGTERM');
    await expect.poll(() => existsSync(first.dir), { timeout: 10_000 }).toBe(false);
  } finally {
    proc.kill('SIGKILL');
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

test('/computers lists the computers opened from anywhere, online or not', async ({ page }) => {
  const host = await remoteHost();
  try {
    const { url } = JSON.parse(host.cli('link', '--anywhere', '--json'));
    await page.goto(url);
    await expectVideo(page);

    // /app/ without a computer is the list; the one just opened is on it, by name, online.
    await page.goto(`${base}/app/`);
    await expect(page).toHaveURL(`${base}/computers`);
    const remote = page.locator('#browser-list li');
    await expect(remote).toHaveCount(1);
    await expect(remote.locator('.detail')).toHaveText('online');
    await expect(remote.locator('.name')).not.toHaveText('Computer');
    expect(await remote.locator('a.open').getAttribute('href')).toMatch(/^\/app\/#r=[A-Za-z0-9_-]+$/);

    // Remote access off: offline. Forgetting it empties the list.
    host.cli('remote', 'off');
    await page.waitForTimeout(4000); // the host notices within ~3 s and disconnects
    await page.reload();
    await expect(remote.locator('.detail')).toHaveText('offline');
    await remote.locator('button.forget').click();
    await expect(remote).toHaveCount(0);
  } finally {
    await host.stop();
  }
});

/** A platform authenticator for this page, that says yes (like Touch ID would). */
async function passkeys(page) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('WebAuthn.enable');
  await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true },
  });
}

test('/computers: a passkey makes an account that signs in and out, and deleting it ends it', async ({ page }) => {
  await passkeys(page);
  await page.goto(`${base}/computers`);
  const note = page.locator('#account-note');
  await page.getByRole('button', { name: 'Create an account' }).click();
  await expect(note).toHaveText('Signed in with a passkey.');
  await page.reload();
  await expect(note).toHaveText('Signed in with a passkey.');

  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(note).toContainText('Sign in with a passkey');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(note).toHaveText('Signed in with a passkey.');

  page.once('dialog', (d) => d.accept());
  await page.getByRole('button', { name: 'Delete account' }).click();
  await expect(note).toContainText('Sign in with a passkey');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.locator('#account-status')).toContainText("isn't on any account");
});

test('/computers: glass account add puts a computer on the account, once, until it takes itself off', async ({ page }) => {
  const host = await remoteHost();
  try {
    host.cli('remote', 'on');
    await passkeys(page);
    await page.goto(`${base}/computers`);
    await page.getByRole('button', { name: 'Create an account' }).click();
    await expect(page.locator('#account-note')).toHaveText('Signed in with a passkey.');
    const mine = page.locator('#mine-list li');
    await expect(page.locator('#mine-note')).toContainText('glass account add');

    const { url, name } = JSON.parse(host.cli('account', 'add', '--json'));
    expect(url).toMatch(new RegExp(`^${base}/computers#claim=[A-Za-z0-9_-]{43}$`));
    await page.goto(url);
    await expect(page.locator('#claim-title')).toHaveText(`Add ${name}?`);
    expect(page.url()).toBe(`${base}/computers`); // the code leaves the address bar
    await page.getByRole('button', { name: `Add ${name}` }).click();
    await expect(page.locator('#claim')).toBeHidden();
    await expect(mine).toHaveCount(1);
    await expect(mine.locator('.name')).toHaveText(name);
    // Online once the host has reached the introducer (it checks every few seconds).
    await expect(async () => {
      await page.reload();
      await expect(mine.locator('.detail')).toHaveText('online', { timeout: 1000 });
    }).toPass({ timeout: 15_000 });
    expect(await mine.locator('a.open').getAttribute('href')).toMatch(/^\/app\/#r=[A-Za-z0-9_-]+$/);

    // One link, one computer, once.
    await page.goto(url);
    await expect(page.locator('#claim-title')).toHaveText("This link doesn't work");
    await page.getByRole('button', { name: 'OK', exact: true }).click();
    await expect(page.locator('#claim')).toBeHidden();

    // The computer takes itself off every account.
    expect(JSON.parse(host.cli('account', 'remove', '--json'))).toEqual({ removed: 1 });
    await page.reload();
    await expect(page.locator('#mine-note')).toContainText('glass account add');
    await expect(mine).toHaveCount(0);

    // Without remote access, there's no link to make.
    host.cli('remote', 'off');
    expect(() => host.cli('account', 'add')).toThrow(/remote access is off/);
  } finally {
    await host.stop();
  }
});

test('/computers: glass fleet on shows what the computer says about itself, and off takes it back', async ({ page }) => {
  const host = await remoteHost();
  try {
    host.cli('remote', 'on');
    await passkeys(page);
    await page.goto(`${base}/computers`);
    await page.getByRole('button', { name: 'Create an account' }).click();
    await expect(page.locator('#account-note')).toHaveText('Signed in with a passkey.');
    const { url, name } = JSON.parse(host.cli('account', 'add', '--json'));
    await page.goto(url);
    await page.getByRole('button', { name: `Add ${name}` }).click();
    const mine = page.locator('#mine-list li');
    await expect(mine).toHaveCount(1);
    await expect(mine.locator('.meta')).toHaveCount(0); // nothing is sent until it's turned on

    expect(host.cli('fleet')).toContain('off');
    expect(host.cli('fleet', 'on')).toContain('every 15 minutes');
    expect(JSON.parse(host.cli('fleet', 'report', '--json'))).toMatchObject({ model: 'MacBook Pro', filevault: true });
    expect(host.cli('fleet')).toContain('last sent');

    await page.reload();
    const meta = mine.locator('.meta');
    await expect(meta).toContainText('MacBook Pro Apple M3 Max');
    await expect(meta).toContainText('64 GB memory');
    await expect(meta).toContainText('FileVault on');
    await expect(meta.locator('.warn')).toHaveText('firewall off'); // the one thing to look at
    await expect(meta).toContainText('reported just now');

    expect(host.cli('fleet', 'off')).toContain('deleted');
    await page.reload();
    await expect(mine.locator('.meta')).toHaveCount(0);
    expect(host.cli('fleet')).toContain('off');
  } finally {
    await host.stop();
  }
});

test('/computers: a passkey you pin lets the account run actions on a computer, and nothing else can', async ({ page }) => {
  const host = await remoteHost();
  try {
    host.cli('remote', 'on');
    await passkeys(page);
    await page.goto(`${base}/computers`);
    await page.getByRole('button', { name: 'Create an account' }).click();
    await expect(page.locator('#account-note')).toHaveText('Signed in with a passkey.');
    const { url, name } = JSON.parse(host.cli('account', 'add', '--json'));
    await page.goto(url);
    await page.getByRole('button', { name: `Add ${name}` }).click();
    const mine = page.locator('#mine-list li');
    await expect(mine).toHaveCount(1);
    host.cli('fleet', 'report');
    // The computer has to be connected to be asked anything.
    await expect(async () => {
      await page.reload();
      await expect(mine.locator('.detail')).toHaveText('online', { timeout: 1000 });
    }).toPass({ timeout: 15_000 });
    // Nothing is set up yet: no Actions button, and the computer refuses a command.
    await expect(mine.locator('button.manage')).toHaveCount(0);
    expect(host.cli('fleet', 'manage', 'status')).toContain('0 passkeys');

    // Pin the passkey with the computer's link; the page vouches for it with a code only the link holds.
    const { url: manageUrl } = JSON.parse(host.cli('fleet', 'manage', '--json'));
    expect(manageUrl).toMatch(new RegExp(`^${base}/computers#manage=[A-Za-z0-9_-]{22}\\.[A-Za-z0-9_-]{22}$`));
    await page.goto(manageUrl);
    expect(page.url()).toBe(`${base}/computers`); // the code leaves the address bar
    await expect(page.locator('#claim-title')).toHaveText(`Let your account manage ${name}?`);
    await page.getByRole('button', { name: 'Allow' }).click();
    await expect(page.locator('#claim')).toBeHidden();
    expect(host.cli('fleet', 'manage', 'status')).toContain('1 passkey can run actions');
    // The link was for one passkey, once.
    await page.goto(manageUrl);
    await expect(page.locator('#claim-title')).toHaveText(`Let your account manage ${name}?`);
    await page.getByRole('button', { name: 'Allow' }).click();
    await expect(page.locator('#claim-status')).toContainText('no enrollment is open');
    await page.getByRole('button', { name: 'Not now' }).click();

    // The computer reports that it can be managed (at once, on pinning); then the page offers actions.
    await expect(async () => {
      await page.reload();
      await expect(mine.locator('button.manage')).toHaveCount(1, { timeout: 1000 });
    }).toPass({ timeout: 20_000 });
    await mine.locator('button.manage').click();
    const status = mine.locator('.act-status');

    // A refresh, signed by the passkey and checked by the computer.
    await mine.getByRole('button', { name: 'Refresh details' }).click();
    await expect(status).toContainText('Details sent', { timeout: 15_000 });

    // A script you placed, run by name; and one you didn't place, or that others can write, doesn't run.
    const scripts = join(host.configDir, 'fleet', 'scripts');
    mkdirSync(scripts, { recursive: true });
    writeFileSync(join(scripts, 'hello.sh'), '#!/bin/sh\necho hello from the fleet\n', { mode: 0o755 });
    writeFileSync(join(scripts, 'loose.sh'), '#!/bin/sh\necho no\n', { mode: 0o777 });
    chmodSync(join(scripts, 'loose.sh'), 0o777);
    let script = 'hello.sh';
    page.on('dialog', (d) => d.accept(d.type() === 'prompt' ? script : undefined));
    await mine.getByRole('button', { name: 'Run a script…' }).click();
    await expect(status).toContainText('hello from the fleet', { timeout: 15_000 });
    script = 'loose.sh';
    await mine.getByRole('button', { name: 'Run a script…' }).click();
    await expect(status).toContainText('can be written by others', { timeout: 15_000 });
    script = 'missing.sh';
    await mine.getByRole('button', { name: 'Run a script…' }).click();
    await expect(status).toContainText('no script missing.sh', { timeout: 15_000 });

    // Everything that ran is in the computer's own log.
    const log = host.cli('fleet', 'manage', 'status');
    expect(log).toContain('report');
    expect(log).toContain('hello.sh');

    // Restart Glass is recorded (test mode doesn't restart), and `off` forgets the passkey.
    await mine.getByRole('button', { name: 'Restart Glass' }).click();
    await expect(status).toContainText('Restarting Glass', { timeout: 15_000 });
    await expect.poll(async () => JSON.stringify(await host.get('/test/records?kind=restart'))).toContain('glass');
    expect(host.cli('fleet', 'manage', 'off')).toContain('1 passkey forgotten');
    await mine.getByRole('button', { name: 'Refresh details' }).click();
    await expect(status).toContainText("isn't set up for actions", { timeout: 15_000 });
  } finally {
    await host.stop();
  }
});

test('/computers: a policy flags the computers that break it, and says how many meet it', async ({ page }) => {
  const host = await remoteHost();
  try {
    host.cli('remote', 'on');
    await passkeys(page);
    await page.goto(`${base}/computers`);
    await page.getByRole('button', { name: 'Create an account' }).click();
    await expect(page.locator('#account-note')).toHaveText('Signed in with a passkey.');
    const { url, name } = JSON.parse(host.cli('account', 'add', '--json'));
    await page.goto(url);
    await page.getByRole('button', { name: `Add ${name}` }).click();
    const mine = page.locator('#mine-list li');
    await expect(mine).toHaveCount(1);
    host.cli('fleet', 'report'); // the fake report: FileVault on, firewall off, 412 of 994 GB free, macOS 15.1
    await page.reload();
    const count = page.locator('#policy-count');
    await expect(count).toHaveText('· none set');
    await expect(mine.locator('.meta')).not.toContainText('Not compliant');

    await page.locator('#policy summary').click();
    await page.locator('#policy-form [name=firewall]').check();
    await page.locator('#policy-form [name=filevault]').check();
    await page.locator('#policy-form [name=os_min]').fill('16');
    await page.locator('#policy-form [name=disk_free_gb]').fill('100');
    await page.getByRole('button', { name: 'Save policy' }).click();
    await expect(page.locator('#policy-status')).toHaveText('Saved.');
    // Firewall is off and macOS 15.1 is older than 16; FileVault is on and the disk has room.
    await expect(mine.locator('.meta .warn', { hasText: 'Not compliant' })).toHaveText('Not compliant: firewall off, system older than 16');
    await expect(count).toHaveText('· 0 of 1 meet it');

    // It's kept: another page load, and searching finds the non-compliant ones.
    await page.reload();
    await expect(page.locator('#policy-form [name=os_min]')).toHaveValue('16');
    await expect(count).toHaveText('· 0 of 1 meet it');

    await page.locator('#policy summary').click();
    await page.locator('#policy-form [name=firewall]').uncheck();
    await page.locator('#policy-form [name=os_min]').fill('15');
    await page.getByRole('button', { name: 'Save policy' }).click();
    await expect(count).toHaveText('· 1 of 1 meet it');
    await expect(mine.locator('.meta')).not.toContainText('Not compliant');
  } finally {
    await host.stop();
  }
});
