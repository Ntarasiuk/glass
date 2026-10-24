// Smooth motion: a viewer on a fast screen (120 Hz) gets more frames; the slowest viewer
// sets the pace, since every viewer shares the stream.

import { Host, expectVideo, stats, test, expect, toolbarClick } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host({ fps: 120 }).start();
    await use(host);
    await host.stop();
  },
});

/** A 120 Hz screen, as far as the page can tell (animation frames stamped 8.3 ms apart). */
async function fastScreen(page) {
  await page.addInitScript(() => {
    // Like a display: frames on a 120 Hz clock, every caller getting the same vsync time.
    const period = 1000 / 120;
    const start = performance.now();
    window.requestAnimationFrame = (cb) => {
      const next = start + (Math.floor((performance.now() - start) / period) + 1) * period;
      return setTimeout(() => cb(next), Math.max(0, next - performance.now()));
    };
  });
}

const fps = async (page) => (await stats(page)).fps;

it('a 120 Hz screen gets more frames, until a 60 Hz viewer joins or it opts out', async ({ browser, host }) => {
  test.setTimeout(90_000);
  const fast = await (await browser.newContext()).newPage();
  await fastScreen(fast);
  await fast.goto(host.url());
  await expectVideo(fast);
  await expect.poll(() => fps(fast), { timeout: 20_000 }).toBeGreaterThan(85);

  // The menu says so, and turning it off goes back to 60.
  await toolbarClick(fast, 'btn-device');
  const item = fast.locator('#device-panel button', { hasText: 'Smooth motion' });
  await expect(item).toContainText('120 Hz');
  await item.click();
  await expect.poll(() => fps(fast), { timeout: 20_000 }).toBeLessThan(66);
  await item.click();
  await expect.poll(() => fps(fast), { timeout: 20_000 }).toBeGreaterThan(85);

  // A viewer on an ordinary screen: everyone gets 60 (the stream is shared).
  const slow = await (await browser.newContext()).newPage();
  await slow.goto(host.url());
  await expectVideo(slow);
  await expect.poll(() => fps(fast), { timeout: 20_000 }).toBeLessThan(66);
  await slow.close();
  await expect.poll(() => fps(fast), { timeout: 20_000 }).toBeGreaterThan(85);
});
