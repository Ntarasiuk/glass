// Sharp text: the viewer asks for detail over motion; the host encodes its displays at
// full size with the stricter text policy, until it's turned off again.

import { Host, expectVideo, hudText, pickMenuItem, test, expect, toolbarClick } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

async function records(host) {
  return (await host.get('/test/records?kind=sharp')).records;
}

/** The decoded size of the first stream, from the HUD ("640×360"). */
async function size(page) {
  return (await hudText(page)).split('\n')[0].match(/(\d+)×(\d+)/)?.[0];
}

it('sharp text sends the display at full size, and auto sizing comes back after', async ({ browser, host }) => {
  // A small window: auto sizing encodes the 640×360 test display smaller.
  const context = await browser.newContext({ viewport: { width: 400, height: 300 } });
  const page = await context.newPage();
  await page.goto(host.url());
  await page.keyboard.press('Control+Shift+H'); // stats, for the HUD size
  await expect.poll(() => size(page), { timeout: 15_000 }).not.toBe('640×360');
  await expect.poll(() => size(page), { timeout: 15_000 }).toMatch(/×/);

  await pickMenuItem(page, 'btn-displays', 'Sharp text');
  await expect.poll(() => records(host)).toEqual([{ on: true, rext: false }]);
  await expect.poll(() => size(page), { timeout: 15_000 }).toBe('640×360');
  await expect.poll(async () => (await hudText(page)).split('\n')[0]).toContain('· sharp');
  expect(await page.evaluate(() => localStorage.getItem('glass.sharp'))).toBe('1');

  // It's this device's setting: a reload asks for it again.
  await page.reload();
  await expect.poll(() => records(host)).toEqual([{ on: true, rext: false }, { on: false, rext: false }, { on: true, rext: false }]);

  await pickMenuItem(page, 'btn-displays', 'Sharp text');
  await expect.poll(() => records(host)).toHaveLength(4);
  expect((await records(host)).at(-1)).toEqual({ on: false, rext: false });
  await expect.poll(() => size(page), { timeout: 15_000 }).not.toBe('640×360');
  await context.close();
});

it('a view-only guest cannot turn it on', async ({ browser, host }) => {
  const made = JSON.parse(host.cli('link', '--json', '--view-only'));
  const code = made.url.match(/#l=([0-9a-f]{32})$/)[1];
  const page = await (await browser.newContext()).newPage();
  await page.goto(`${host.base}/#l=${code}`);
  await expectVideo(page);
  await toolbarClick(page, 'btn-displays');
  await expect(page.locator('.panel:not(.hidden) button', { hasText: 'Sharp text' })).toHaveCount(0);
  expect(await records(host)).toEqual([]);
});
