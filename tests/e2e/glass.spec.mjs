// End-to-end tests against real `glass --test` hosts in Google Chrome.
// The host streams a test pattern on two fake 640×360 displays and records input.

import { test, expect } from '@playwright/test';
import { Host, expectVideo, hudText, pickMenuItem, stats, touch } from './harness.mjs';

/** One host per test, stopped afterwards. */
const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

/** Mac virtual keycodes the assertions refer to. */
const K = { A: 0x00, S: 0x01, H: 0x04, I: 0x22, V: 0x09, C: 0x08, Tab: 0x30, Shift: 0x38, Cmd: 0x37, Ctrl: 0x3b };

async function open(page, host, options) {
  await page.goto(host.url(options));
  await expectVideo(page);
}

async function keys(host, type) {
  return (await host.input(type)).events.map((e) => e.keycode);
}

test.describe('connecting', () => {
  it('decodes video with low latency', async ({ page, host }) => {
    await open(page, host);
    await expect.poll(async () => (await stats(page)).fps, { timeout: 10_000 }).toBeGreaterThan(15);
    // Judge a few seconds of one-second samples: one busy second on a loaded test machine
    // shouldn't fail the test, a consistently slow pipeline should.
    const samples = [];
    for (let i = 0; i < 4; i++) {
      await page.waitForTimeout(1000);
      samples.push(await stats(page));
    }
    const latencies = samples.map((s) => s.latency).sort((a, b) => a - b);
    expect(latencies[0]).toBeGreaterThan(0);
    expect(latencies[1]).toBeLessThan(100);
    expect(samples.every((s) => s.dropped === 0)).toBeTruthy();
  });

  it('removes the token from the URL and remembers it', async ({ page, host }) => {
    await open(page, host);
    expect(page.url()).not.toContain('#t=');
    await page.reload();
    await expectVideo(page);
  });

  it('asks for a token, rejects a wrong one, accepts the right one', async ({ page, host }) => {
    await page.goto(host.url({ token: false }));
    const auth = page.locator('#auth');
    await expect(auth).toBeVisible();
    await expect(page.locator('#screens canvas').first()).toBeAttached();
    expect(await hudText(page)).not.toContain('640×360');

    await auth.locator('input').fill('not-the-token');
    await auth.locator('button').click();
    await expect(auth).toBeVisible({ timeout: 10_000 });

    await auth.locator('input').fill(host.token);
    await auth.locator('button').click();
    await expectVideo(page);
    await expect(auth).toBeHidden();
  });

  it('sends nothing to an unauthenticated session', async ({ page, host }) => {
    await page.goto(`${host.base}/config.json`); // a same-origin page that doesn't run the client
    const result = await page.evaluate(async (wt) => {
      const config = await fetch('/config.json').then((r) => r.json());
      const t = new WebTransport(`https://localhost:${wt}/`, {
        serverCertificateHashes: [{ algorithm: 'sha-256', value: new Uint8Array(config.certHash) }],
      });
      await t.ready;
      // Input before hello must be ignored.
      const input = await t.createUnidirectionalStream();
      const w = input.getWriter();
      await w.write(new Uint8Array([0x49, 5, 0, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
      const reader = t.datagrams.readable.getReader();
      const got = await Promise.race([
        reader.read().then(() => 'datagram'),
        new Promise((r) => setTimeout(() => r('none'), 1500)),
      ]);
      t.close();
      return got;
    }, host.wt);
    expect(result).toBe('none');
    expect((await host.input()).events).toHaveLength(0);
  });

  it('serves two clients at once', async ({ browser, host }) => {
    const [a, b] = [await browser.newPage(), await browser.newPage()];
    await open(a, host);
    await open(b, host);
    await expect.poll(async () => (await stats(a)).fps).toBeGreaterThan(10);
    await expect.poll(async () => (await stats(b)).fps).toBeGreaterThan(10);
  });
});

test.describe('displays', () => {
  it('lists displays and switches between one and all', async ({ page, host }) => {
    await open(page, host);
    await page.locator('#btn-displays').click();
    const items = page.locator('#displays-panel button');
    await expect(items).toHaveCount(3);
    await expect(items.last()).toContainText('All displays');
    await items.last().click();
    await expectVideo(page, 2);
    await expect(page.locator('#screens canvas:not(.hidden)')).toHaveCount(2);

    await pickMenuItem(page, 'btn-displays', 'Right display');
    await expectVideo(page, 1);
    await expect(page.locator('#screens canvas:not(.hidden)')).toHaveCount(1);
  });
});

test.describe('input', () => {
  it('maps mouse position and clicks onto the right display', async ({ page, host }) => {
    await open(page, host);
    await pickMenuItem(page, 'btn-displays', 'All displays');
    await expectVideo(page, 2);
    await host.clearInput();

    const canvases = page.locator('#screens canvas:not(.hidden)');
    for (const [i, canvas] of [canvases.nth(0), canvases.nth(1)].entries()) {
      const box = await canvas.boundingBox();
      // Letterboxed: the 16:9 video sits centred inside the canvas box.
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await page.mouse.down();
      await page.mouse.up();
      await expect.poll(async () => (await host.input('move')).events.filter((e) => e.stream === i).length).toBeGreaterThan(0);
      const move = (await host.input('move')).events.filter((e) => e.stream === i).at(-1);
      expect(move.x).toBeCloseTo(0.5, 1);
      expect(move.y).toBeCloseTo(0.5, 1);
    }
    const { events } = await host.input();
    expect(events.filter((e) => e.t === 'down' && e.button === 0)).toHaveLength(2);
    expect(events.filter((e) => e.t === 'up' && e.button === 0)).toHaveLength(2);
  });

  it('sends right clicks and wheel scrolling', async ({ page, host }) => {
    await open(page, host);
    const box = await page.locator('#screens canvas').first().boundingBox();
    await page.mouse.move(box.x + 100, box.y + 100);
    await host.clearInput();
    await page.mouse.click(box.x + 100, box.y + 100, { button: 'right' });
    await page.mouse.wheel(0, 120);
    await expect.poll(async () => (await host.input('scroll')).events.length).toBeGreaterThan(0);
    const { events } = await host.input();
    expect(events.some((e) => e.t === 'down' && e.button === 2)).toBeTruthy();
    expect((await host.input('scroll')).events.reduce((sum, e) => sum + e.dy, 0)).toBeCloseTo(120, 0);
  });

  it('sends typed keys as Mac keycodes', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    // Like a real keyboard: Shift is its own key press.
    await page.keyboard.press('Shift+KeyH');
    await page.keyboard.press('KeyI');
    await expect.poll(() => keys(host, 'keyup')).toEqual([K.H, K.Shift, K.I]);
    expect(await keys(host, 'keydown')).toEqual([K.Shift, K.H, K.I]);
  });

  it('PC keyboard mode turns Ctrl into ⌘ and Super into Ctrl', async ({ page, host }) => {
    await page.goto(host.url());
    await page.evaluate(() => localStorage.setItem('glass.pcKeyboard', '1'));
    await page.reload();
    await expectVideo(page);
    await host.clearInput();
    await page.keyboard.press('Control+S');
    await page.keyboard.press('Meta');
    await expect.poll(() => keys(host, 'keydown')).toEqual([K.Cmd, K.S, K.Ctrl]);
  });

  it('Ctrl as ⌘ turns Ctrl+Tab into ⌘Tab and keeps other Ctrl combos', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    await page.keyboard.down('Control');
    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    await page.keyboard.up('Control');
    await page.keyboard.press('Control+A');
    await expect.poll(() => keys(host, 'keydown')).toEqual([K.Ctrl, K.Cmd, K.Tab, K.Tab, K.Ctrl, K.A]);
    // The ⌘ stand-in is released with Ctrl.
    expect((await keys(host, 'keyup')).filter((k) => k === K.Cmd)).toHaveLength(1);
  });

  test('a Linux host gets keys as typed and no Mac shortcuts menu', async ({ page }) => {
    const host = await new Host({ os: 'linux' }).start();
    try {
      await page.goto(host.url());
      await page.evaluate(() => localStorage.setItem('glass.pcKeyboard', '1'));
      await page.reload();
      await expectVideo(page);
      await expect(page.locator('#btn-shortcuts')).toBeHidden();
      await host.clearInput();
      // PC keyboard mode and Ctrl as ⌘ only make sense for Mac hosts.
      await page.keyboard.press('Control+S');
      await page.keyboard.down('Control');
      await page.keyboard.press('Tab');
      await page.keyboard.up('Control');
      await expect.poll(() => keys(host, 'keydown')).toEqual([K.Ctrl, K.S, K.Ctrl, K.Tab]);
    } finally {
      await host.stop();
    }
  });

  it('touch: tap clicks, long press right-clicks, two fingers scroll', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    await touch(page, 0, [{ type: 'pointerdown', fx: 0.25, fy: 0.25 }, { type: 'pointerup', fx: 0.25, fy: 0.25 }]);
    await expect.poll(async () => (await host.input('up')).events.length).toBe(1);
    expect((await host.input('down')).events[0].button).toBe(0);

    await host.clearInput();
    await touch(page, 0, [{ type: 'pointerdown', fx: 0.5, fy: 0.5 }]);
    await page.waitForTimeout(700);
    await touch(page, 0, [{ type: 'pointerup', fx: 0.5, fy: 0.5 }]);
    await expect.poll(async () => (await host.input('down')).events.map((e) => e.button)).toContain(2);

    await host.clearInput();
    await touch(page, 0, [
      { type: 'pointerdown', id: 1, fx: 0.4, fy: 0.6 },
      { type: 'pointerdown', id: 2, fx: 0.6, fy: 0.6 },
      { type: 'pointermove', id: 1, fx: 0.4, fy: 0.4 },
      { type: 'pointermove', id: 2, fx: 0.6, fy: 0.4 },
      { type: 'pointerup', id: 1, fx: 0.4, fy: 0.4 },
      { type: 'pointerup', id: 2, fx: 0.6, fy: 0.4 },
    ]);
    await expect.poll(async () => (await host.input('scroll')).events.length).toBeGreaterThan(0);
    // Fingers moved up, so content scrolls down (positive dy), and nothing was clicked.
    expect((await host.input('scroll')).events.reduce((s, e) => s + e.dy, 0)).toBeGreaterThan(0);
    expect((await host.input('down')).events).toHaveLength(0);
  });

  it('on-screen keyboard text becomes key taps', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    await page.locator('#keys').evaluate((el) => {
      el.dispatchEvent(new InputEvent('beforeinput', { inputType: 'insertText', data: 'a!', bubbles: true, cancelable: true }));
      el.dispatchEvent(new InputEvent('beforeinput', { inputType: 'deleteContentBackward', bubbles: true, cancelable: true }));
    });
    // a, then ! = Shift+1, then Backspace.
    await expect.poll(() => keys(host, 'keydown')).toEqual([K.A, K.Shift, 0x12, 0x33]);
  });
});

test.describe('clipboard and files', () => {
  it('syncs the clipboard in both directions', async ({ page, host }) => {
    await open(page, host);
    await host.post('/test/clipboard', 'copied on the host');
    await expect.poll(() => host.clientLog('mac clipboard changed').length).toBeGreaterThan(0);

    await host.clearInput();
    await page.evaluate(() => {
      const data = new DataTransfer();
      data.setData('text/plain', 'pasted from the client');
      document.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    });
    await expect.poll(() => host.get('/test/clipboard')).toBe('pasted from the client');
    expect((await host.input()).pastes).toBe(1);
  });

  it('uploads files to the host', async ({ page, host }) => {
    await open(page, host);
    const body = Buffer.alloc(3 * 1024 * 1024, 7);
    await page.locator('#file-input').setInputFiles([
      { name: 'big.bin', mimeType: 'application/octet-stream', buffer: body },
      { name: '../../escape.txt', mimeType: 'text/plain', buffer: Buffer.from('hi') },
    ]);
    const files = async () => Object.fromEntries((await host.get('/test/downloads')).files.map((f) => [f.name, f.size]));
    // Uploads land under a .glass-partial name and are renamed when complete.
    await expect.poll(files, { timeout: 20_000 }).toEqual({ 'big.bin': body.length, 'escape.txt': 2 });
    // "../../escape.txt" had its path stripped: it landed in the downloads folder.
    await expect(page.locator('.toast', { hasText: 'Saved to Mac' }).first()).toBeVisible();
  });

  it('downloads files picked on the host', async ({ page, host }) => {
    await open(page, host);
    await page.locator('#btn-download').click();
    // The finished transfer's toast has the Save button (the progress toast doesn't).
    const toast = page.locator('.toast', { hasText: 'glass-test-pick', has: page.locator('button') });
    await expect(toast).toBeVisible({ timeout: 10_000 });
    await expect(toast.locator('button', { hasText: 'Save' })).toBeVisible();
  });
});

test.describe('audio', () => {
  it('streams audio once turned on', async ({ page, host }) => {
    await open(page, host);
    await expect(page.locator('#btn-audio')).toBeVisible();
    await page.locator('#btn-audio').click();
    await expect.poll(async () => (await stats(page)).audioPackets, { timeout: 10_000 }).toBeGreaterThan(150);
  });
});

test.describe('resilience', () => {
  it('recovers from lost frames with a keyframe', async ({ page, host }) => {
    await open(page, host);
    await host.post('/test/drop?frames=3');
    await expect.poll(async () => (await stats(page)).dropped, { timeout: 5_000 }).toBeGreaterThan(0);
    await expect.poll(async () => (await stats(page)).dropped, { timeout: 5_000 }).toBe(0);
    expect((await stats(page)).fps).toBeGreaterThan(10);
    expect(host.clientLog('decoder error detail')).toHaveLength(0);
  });

  it('never decodes a corrupted frame', async ({ page, host }) => {
    await open(page, host);
    await host.post('/test/corrupt');
    await expect.poll(() => host.clientLog('frame checksum mismatch').length).toBe(1);
    await expect.poll(async () => (await stats(page)).fps).toBeGreaterThan(10);
    expect(host.clientLog('decoder error detail')).toHaveLength(0);
  });

  it('reconnects after the host restarts', async ({ page, host }) => {
    await open(page, host);
    await host.stop();
    await expect.poll(async () => (await stats(page)).connected, { timeout: 10_000 }).toBe(false);
    await host.start();
    await expect.poll(async () => (await stats(page)).fps, { timeout: 20_000 }).toBeGreaterThan(10);
  });
});

test.describe('machines', () => {
  it('switches to another machine and falls back from a dead one', async ({ page, host }) => {
    // Shares the config dir, so the same token and certificate work on both.
    const other = await new Host({ configDir: host.configDir }).start();
    try {
      await open(page, host);
      await page.evaluate((id) => localStorage.setItem('glass.machine', id), `localhost:${other.wt}`);
      await page.reload();
      await expectVideo(page);
      await host.clearInput();
      await other.clearInput();
      await page.keyboard.press('a');
      await expect.poll(async () => (await other.input('keydown')).events.length).toBe(1);
      expect((await host.input('keydown')).events).toHaveLength(0);

      // A machine that never answers: back home after a few tries.
      await page.evaluate(() => localStorage.setItem('glass.machine', 'localhost:1'));
      await page.reload();
      await expect(page.locator('.toast', { hasText: 'back to this machine' })).toBeVisible({ timeout: 30_000 });
      await expectVideo(page);
    } finally {
      await other.stop();
    }
  });
});

test.describe('updates', () => {
  it('reloads into new client code after an update', async ({ page, host }) => {
    await open(page, host);
    const { version } = await host.get('/config.json');
    // Simulate an update by bumping a web file's modification time.
    const { utimesSync } = await import('node:fs');
    const file = new URL('../../web/icon.svg', import.meta.url);
    const now = new Date();
    utimesSync(file, now, now);
    expect((await host.get('/config.json')).version).not.toBe(version);
    const reloaded = page.waitForEvent('load', { timeout: 20_000 });
    await host.stop();
    await host.start();
    await reloaded;
    await expectVideo(page);
  });
});
