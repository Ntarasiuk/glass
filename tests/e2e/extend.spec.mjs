// "Use as a second display": the viewer asks for a display in its screen's shape (test mode
// records the request instead of adding a real one), and turning it off asks for it to go.

import { Host, expectVideo, test, expect } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

async function requests(host) {
  return (await host.get('/test/records?kind=extend')).records;
}

it('asks for a display in this screen\'s shape, then removes it', async ({ browser, host }) => {
  const page = await (await browser.newContext({ viewport: { width: 1180, height: 820 }, deviceScaleFactor: 2 })).newPage();
  await page.goto(host.url());
  await expectVideo(page);
  await page.locator('#btn-displays').dispatchEvent('click');
  const item = page.locator('.panel:not(.hidden) button', { hasText: 'Use as a second display' });
  await expect(item).toHaveAttribute('aria-checked', 'false');
  await item.dispatchEvent('click');
  await expect.poll(() => requests(host)).toEqual([{ size: [2360, 1640], hidpi: true }]);

  await page.locator('#btn-displays').dispatchEvent('click');
  await expect(item).toHaveAttribute('aria-checked', 'true');
  await item.dispatchEvent('click');
  await expect.poll(() => requests(host)).toEqual([{ size: [2360, 1640], hidpi: true }, { size: null, hidpi: false }]);
});

it('is the owner\'s alone: a guest link doesn\'t offer it', async ({ browser, host }) => {
  const made = JSON.parse(host.cli('link', '--json', '--for', '1h'));
  const page = await (await browser.newContext()).newPage();
  await page.goto(`${host.base}/#l=${made.url.match(/#l=([0-9a-f]{32})$/)[1]}`);
  await expectVideo(page);
  await page.locator('#btn-displays').dispatchEvent('click');
  await expect(page.locator('.panel:not(.hidden) button', { hasText: 'Auto' })).toBeVisible();
  await expect(page.locator('.panel:not(.hidden) button', { hasText: 'Use as a second display' })).toHaveCount(0);
});
