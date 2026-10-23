// Keyboard remaps remembered per machine, and the shortcut bar.

import { Host, expectVideo, test, expect, toolbarClick } from './harness.mjs';

const K = { Tab: 0x30, Cmd: 0x37 };

async function checked(page, label) {
  await toolbarClick(page, 'btn-shortcuts');
  const v = await page.locator('#shortcuts-panel button', { hasText: label }).first().getAttribute('aria-checked');
  await page.keyboard.press('Escape');
  return v;
}

async function toggle(page, label) {
  await toolbarClick(page, 'btn-shortcuts');
  await page.locator('#shortcuts-panel button', { hasText: label }).first().click();
  await page.keyboard.press('Escape');
}

test('each machine keeps its own keyboard remaps', async ({ page }) => {
  const host = await new Host().start();
  try {
    await page.goto(host.url());
    await expectVideo(page);
    expect(await checked(page, 'Ctrl as ⌘')).toBe('true');
    await toggle(page, 'Ctrl as ⌘');
    await toggle(page, 'Swap ⌘ and ⌥');
    // Saved under this machine (host and port), alongside another machine's own profile.
    const id = `localhost:${host.wt}`;
    const saved = await page.evaluate((id) => JSON.parse(localStorage.getItem(`glass.keyboard.${id}`)), id);
    expect(saved).toEqual({ pc: false, ctrlAsCmd: false, swap: true });
    await page.evaluate((id) => localStorage.setItem(`glass.keyboard.${id}`, JSON.stringify({ pc: true, ctrlAsCmd: true, swap: false })), id);
    // On reconnect the machine's profile is what applies, not the last toggles.
    await page.reload();
    await expectVideo(page);
    expect(await checked(page, 'PC keyboard')).toBe('true');
    expect(await checked(page, 'Ctrl as ⌘')).toBe('true');
    expect(await checked(page, 'Swap ⌘ and ⌥')).toBe('false');
  } finally {
    await host.stop();
  }
});

test('the shortcut bar sends shortcuts with one tap', async ({ page }) => {
  const host = await new Host().start();
  try {
    await page.goto(host.url());
    await expectVideo(page);
    await expect(page.locator('#shortcut-bar')).toBeHidden();
    await toggle(page, 'Shortcut bar');
    await expect(page.locator('#shortcut-bar')).toBeVisible();
    await host.clearInput();
    await page.locator('#shortcut-bar button[title="App switcher"], #shortcut-bar button').first().click();
    await expect.poll(async () => (await host.input('keydown')).events.length).toBeGreaterThan(0);
    // Remembered across reloads.
    await page.reload();
    await expectVideo(page);
    await expect(page.locator('#shortcut-bar')).toBeVisible();
  } finally {
    await host.stop();
  }
});
