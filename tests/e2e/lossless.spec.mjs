// Sharp text's lossless refresh: once the screen is still, a viewer with sharp text on gets
// the exact picture (PNG, full colour) on top of the video, until the screen changes.

import { Host, expectVideo, hudText, pickMenuItem, test, expect } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.post('/test/pause?on=false');
    await host.stop();
  },
});

const refreshes = (host) => host.clientLog('lossless refresh').map((e) => e.data);
const hudLine = async (page) => (await hudText(page)).split('\n')[0];

// Display 0's background and the sweeping bar, in RGB, exactly.
const PATTERN = [[30, 80, 200], [255, 255, 255]];

it('a still screen arrives losslessly with sharp text, and only the changed part after', async ({ page, host }) => {
  await page.goto(host.url());
  await expectVideo(page);
  await page.keyboard.press('Control+Shift+H'); // stats, for the HUD
  await host.post('/test/pause?on=true');
  await page.waitForTimeout(1500);
  expect(refreshes(host), 'none without sharp text').toEqual([]);

  await page.mouse.move(640, 10); // the toolbar tucked itself away while we waited
  await page.waitForTimeout(600);
  await pickMenuItem(page, 'btn-displays', 'Sharp text');
  await expect.poll(() => refreshes(host).length, { timeout: 15_000 }).toBeGreaterThan(0);
  const [first] = refreshes(host);
  expect(first).toMatchObject({ stream: 0, x: 0, y: 0, w: 640, h: 360, fw: 640, fh: 360 });
  // The pixel on screen is exactly the pattern's colour: no video blur, no 4:2:0.
  expect(PATTERN).toContainEqual(first.pixel.slice(2));
  await expect.poll(() => hudLine(page)).toContain('4:4:4');

  // Motion takes over from the still picture; when it stops, only the bar's area is sent.
  await host.post('/test/pause?on=false');
  await expect.poll(() => hudLine(page)).not.toContain('4:4:4');
  await host.post('/test/pause?on=true');
  await expect.poll(() => refreshes(host).length, { timeout: 15_000 }).toBeGreaterThan(1);
  const second = refreshes(host).at(-1);
  expect(second.w).toBeLessThan(640);
  expect(second.h).toBe(360);
  expect(PATTERN).toContainEqual(second.pixel.slice(2));
  await expect.poll(() => hudLine(page)).toContain('4:4:4');
});
