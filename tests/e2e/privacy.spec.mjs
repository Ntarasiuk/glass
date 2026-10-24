// The computer menu: privacy screen and restarts, owner only.

import { Host, expectVideo, pickMenuItem, test, expect, toolbarClick } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

const records = async (host, kind) => (await host.get(`/test/records?kind=${kind}`)).records;

it('turns the privacy screen on and off from the viewer', async ({ page, host }) => {
  await page.goto(host.url());
  await expectVideo(page);
  await pickMenuItem(page, 'btn-more', 'Privacy screen');
  await expect.poll(() => records(host, 'privacy')).toEqual([true]);
  // The host confirms, and the menu shows it.
  await toolbarClick(page, 'btn-more');
  await expect(page.locator('#more-panel button', { hasText: 'Privacy screen' })).toHaveAttribute('aria-checked', 'true');
  await page.locator('#more-panel button', { hasText: 'Privacy screen' }).click();
  await expect.poll(() => records(host, 'privacy')).toEqual([true, false]);
});

it('turns the privacy screen off when the viewer leaves', async ({ page, host }) => {
  await page.goto(host.url());
  await expectVideo(page);
  await pickMenuItem(page, 'btn-more', 'Privacy screen');
  await expect.poll(() => records(host, 'privacy')).toEqual([true]);
  await page.close();
  await expect.poll(() => records(host, 'privacy')).toEqual([true, false]);
});

it('restarts Glass or the computer after confirming, and says so', async ({ page, host }) => {
  await page.goto(host.url());
  await expectVideo(page);
  // Declining the confirm sends nothing.
  page.once('dialog', (d) => d.dismiss());
  await pickMenuItem(page, 'btn-more', 'Restart computer');
  page.once('dialog', (d) => d.accept());
  await pickMenuItem(page, 'btn-more', 'Restart Glass');
  await expect.poll(() => records(host, 'restart')).toEqual(['glass']);
  await expect(page.locator('#status-title')).toHaveText('Restarting…');
  page.once('dialog', (d) => d.accept());
  await pickMenuItem(page, 'btn-more', 'Restart computer');
  await expect.poll(() => records(host, 'restart')).toEqual(['glass', 'computer']);
});

it("doesn't offer the computer menu to guests", async ({ browser, host }) => {
  const made = JSON.parse(host.cli('link', '--json', '--for', '1h'));
  const code = made.url.match(/#l=([0-9a-f]{32})$/)[1];
  const context = await browser.newContext();
  const guest = await context.newPage();
  await guest.goto(`${host.base}/#l=${code}`);
  await expectVideo(guest);
  // A guest's … menu has the stats, and none of the computer's controls.
  await guest.locator('#btn-more').click();
  await expect(guest.locator('#more-panel button', { hasText: 'Stats' })).toBeVisible();
  await expect(guest.locator('#more-panel button', { hasText: /Privacy|Restart|Console/ })).toHaveCount(0);
  await context.close();
});
