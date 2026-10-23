// Game controllers and the viewer's microphone, against a `glass --test` host.

import { test, expect } from '@playwright/test';
import { Host, expectVideo } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

test.use({
  permissions: ['microphone'],
  launchOptions: {
    args: ['--autoplay-policy=no-user-gesture-required', '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream'],
  },
});

/** A scriptable controller behind navigator.getGamepads(). */
function fakeGamepad() {
  const pad = {
    id: 'Xbox Wireless Controller (STANDARD GAMEPAD)',
    index: 0,
    mapping: 'standard',
    connected: true,
    buttons: Array.from({ length: 17 }, () => ({ pressed: false, value: 0 })),
    axes: [0, 0, 0, 0],
  };
  let pads = [];
  navigator.getGamepads = () => pads;
  document.hasFocus = () => true;
  window.__pad = {
    connect() {
      pads = [pad];
      const e = new Event('gamepadconnected');
      Object.defineProperty(e, 'gamepad', { value: pad });
      dispatchEvent(e);
    },
    press(i, value = 1) {
      pad.buttons[i] = { pressed: value > 0.5, value };
    },
    tilt(i, v) {
      pad.axes[i] = v;
    },
  };
}

const has = (events, kind, index, value) =>
  events.some((e) => e.kind === kind && e.index === index && Math.abs(e.value - value) < 1e-3);

test.describe('controllers', () => {
  it('sends button and stick changes to the host', async ({ page, host }) => {
    await page.addInitScript(fakeGamepad);
    await page.goto(host.url());
    await expectVideo(page);
    await page.evaluate(() => window.__pad.connect());
    await expect(page.locator('.toast', { hasText: 'Controller connected' })).toBeVisible();
    await page.evaluate(() => {
      window.__pad.press(0);
      window.__pad.press(7, 0.5);
      window.__pad.tilt(1, -1);
    });
    await expect
      .poll(async () => {
        const { events } = await host.input('gamepad');
        return has(events, 2, 0, 1) && has(events, 0, 0, 1) && has(events, 0, 7, 0.5) && has(events, 1, 1, -1);
      })
      .toBe(true);
    // Unchanged state sends nothing more.
    const before = (await host.input('gamepad')).events.length;
    await page.waitForTimeout(300);
    expect((await host.input('gamepad')).events.length).toBe(before);
  });

  it('are left out of view-only links', async ({ page, host }) => {
    // `glass link` needs the host's status file, written within a second of starting.
    let url;
    await expect
      .poll(() => {
        try {
          url = JSON.parse(host.cli('link', '--json', '--view-only')).url;
          return true;
        } catch {
          return false;
        }
      })
      .toBe(true);
    await page.addInitScript(fakeGamepad);
    await page.goto(`${host.base}/${url.slice(url.indexOf('#'))}`);
    await expectVideo(page);
    await page.evaluate(() => {
      window.__pad.connect();
      window.__pad.press(0);
    });
    await page.waitForTimeout(500);
    expect((await host.input('gamepad')).events).toEqual([]);
    await expect(page.locator('#btn-mic')).toBeHidden();
  });
});

test.describe('microphone', () => {
  it('streams the viewer microphone to the host while on', async ({ page, host }) => {
    await page.goto(host.url());
    await expectVideo(page);
    const mic = page.locator('#btn-mic');
    await expect(mic).toBeVisible();
    await mic.click();
    await expect(mic).toHaveAttribute('aria-pressed', 'true');
    const records = async () => (await host.get('/test/records?kind=mic')).records;
    // 5 ms packets: plenty within a couple of seconds.
    await expect.poll(async () => (await records()).length, { timeout: 10_000 }).toBeGreaterThan(50);
    expect((await records()).every((n) => n === 16 + 240 * 2)).toBe(true);
    await mic.click();
    await expect(mic).toHaveAttribute('aria-pressed', 'false');
    await page.waitForTimeout(300);
    const stopped = (await records()).length;
    await page.waitForTimeout(500);
    expect((await records()).length).toBe(stopped);
  });
});
