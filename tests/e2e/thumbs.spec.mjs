// Display previews: a strip of every display along the bottom when there's room below the
// picture, the shown one marked; tap to switch; hideable, and remembered.

import { Host, expectVideo, test, expect } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

it('shows each display below the picture and switches between them', async ({ browser, host }) => {
  // Taller than the 16:9 test displays: a band free below the picture.
  const context = await browser.newContext({ viewport: { width: 1200, height: 900 } });
  const page = await context.newPage();
  await page.goto(host.url());
  await expectVideo(page);
  const strip = page.locator('#thumbs');
  await expect(strip).toBeVisible();
  const thumbs = strip.locator('.thumb');
  await expect(thumbs).toHaveCount(3); // two displays, then All
  await expect(thumbs.nth(0)).toHaveAttribute('aria-current', 'true');
  // Pictures of both, including the one not being shown.
  await expect(strip.locator('.thumb img[src^="data:image/png"]')).toHaveCount(2);

  await thumbs.nth(1).click();
  await expect(thumbs.nth(1)).toHaveAttribute('aria-current', 'true');
  await expect(thumbs.nth(0)).not.toHaveAttribute('aria-current', 'true');

  await thumbs.nth(2).click();
  await expect(thumbs.nth(2)).toHaveAttribute('aria-current', 'true');
  await expect(page.locator('#screens canvas:not(.hidden)')).toHaveCount(2);

  // Hidden: a tab brings it back; remembered across reloads.
  await page.locator('#thumbs-hide').click();
  await expect(strip).toBeHidden();
  await expect(page.locator('#thumbs-show')).toBeVisible();
  await page.reload();
  await expectVideo(page, 2); // still showing All
  await expect(page.locator('#thumbs-show')).toBeVisible();
  await expect(strip).toBeHidden();
  await page.locator('#thumbs-show').click();
  await expect(strip).toBeVisible();
});

it('stays tucked behind its tab when the picture fills the bottom', async ({ browser, host }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 820 } });
  const page = await context.newPage();
  await page.goto(host.url());
  await expectVideo(page);
  await expect(page.locator('#thumbs-show')).toBeVisible();
  await expect(page.locator('#thumbs')).toBeHidden();
  // Opened anyway, it shows over the picture.
  await page.locator('#thumbs-show').click();
  await expect(page.locator('#thumbs')).toBeVisible();
  await expect(page.locator('#thumbs')).toHaveClass(/over/);
});
