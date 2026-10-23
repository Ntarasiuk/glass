// Custom shortcuts in the ⌘ menu, and viewer branding from `glass config name/accent`.

import { test, expect } from '@playwright/test';
import { Host, expectVideo, pickMenuItem } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

const K = { Shift: 0x38, Cmd: 0x37, Four: 0x15 };

it('records a shortcut, sends it from the menu, and removes it', async ({ page, host }) => {
  await page.goto(host.url());
  await expectVideo(page);
  page.on('dialog', (d) => d.accept('My grab'));
  await pickMenuItem(page, 'btn-shortcuts', 'Add shortcut');
  await expect(page.locator('#shortcuts-panel button', { hasText: 'Press a shortcut' })).toBeVisible();
  await host.clearInput();
  await page.keyboard.press('Meta+Shift+Digit4');
  // Recording swallows the combo: nothing reaches the host.
  await expect(page.locator('#shortcuts-panel button', { hasText: 'My grab' })).toBeVisible();
  expect((await host.input('keydown')).events).toHaveLength(0);
  expect(await page.evaluate(() => localStorage.getItem('glass.customShortcuts'))).toContain('Digit4');

  await page.locator('#shortcuts-panel button', { hasText: 'My grab' }).click();
  await expect.poll(async () => (await host.input('keydown')).events.map((e) => e.keycode)).toEqual([K.Shift, K.Cmd, K.Four]);

  // Kept across reloads, and removable.
  await page.reload();
  await expectVideo(page);
  await page.locator('#btn-shortcuts').click();
  await page.getByRole('button', { name: 'Remove My grab' }).click();
  await expect(page.locator('#shortcuts-panel button', { hasText: 'My grab' })).toHaveCount(0);
  expect(await page.evaluate(() => localStorage.getItem('glass.customShortcuts'))).toBe('[]');
});

it('Escape cancels recording', async ({ page, host }) => {
  await page.goto(host.url());
  await expectVideo(page);
  await pickMenuItem(page, 'btn-shortcuts', 'Add shortcut');
  await page.keyboard.press('Escape');
  await expect(page.locator('#shortcuts-panel button', { hasText: 'Add shortcut' })).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem('glass.customShortcuts'))).toBe('[]');
});

it('shows the name and accent the owner configured', async ({ page, host }) => {
  host.cli('config', 'name', 'Studio Mac');
  host.cli('config', 'accent', '#ffcc00');
  await page.goto(host.url());
  await expectVideo(page);
  await expect(page).toHaveTitle('Studio Mac');
  const style = await page.evaluate(() => [
    document.documentElement.style.getPropertyValue('--accent'),
    document.documentElement.style.getPropertyValue('--accent-ink'),
  ]);
  // A light accent gets dark ink.
  expect(style).toEqual(['#ffcc00', '#04121f']);
});
