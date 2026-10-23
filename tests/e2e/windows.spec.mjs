// Single-app streaming: the displays menu lists the host's windows; picking one streams
// just that window (the test host's fake windows are 480×320) and input maps onto it.

import { Host, expectVideo, hudText, pickMenuItem, test, expect, toolbarClick } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

/** How many decoded streams the HUD shows at `size`. */
async function streamsAt(page, size) {
  return ((await hudText(page)).split('\n')[0].match(new RegExp(size, 'g')) ?? []).length;
}

test.describe('apps', () => {
  it('lists windows and streams one on its own', async ({ page, host }) => {
    await page.goto(host.url());
    await expectVideo(page);

    await toolbarClick(page, 'btn-displays');
    const panel = page.locator('#displays-panel');
    await expect(panel).toContainText('Apps');
    await expect(panel.locator('button', { hasText: 'Notes' })).toContainText('Notes — todo.txt');
    await expect(panel.locator('button', { hasText: 'Terminal' })).toBeVisible();
    await panel.locator('button', { hasText: 'Notes' }).click();

    await expect.poll(() => streamsAt(page, '480×320'), { timeout: 15_000 }).toBe(1);
    await expect.poll(() => streamsAt(page, '640×360')).toBe(0);

    // The window is checked, and not listed as a display.
    await toolbarClick(page, 'btn-displays');
    await expect(panel.locator('button[aria-checked="true"]', { hasText: 'Notes' })).toBeVisible();
    await expect(panel.locator('button', { hasText: 'display' })).toHaveCount(3); // Left, Right, All
    await page.keyboard.press('Escape');

    // Input lands on stream 0, which now carries the window.
    await host.clearInput();
    const box = await page.locator('#screens canvas:not(.hidden)').first().boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await expect.poll(async () => (await host.input('move')).events.length).toBeGreaterThan(0);
    const move = (await host.input('move')).events.at(-1);
    expect(move.stream).toBe(0);
    expect(move.x).toBeCloseTo(0.5, 1);
    expect(move.y).toBeCloseTo(0.5, 1);

    // Back to a display.
    await pickMenuItem(page, 'btn-displays', 'Left display');
    await expectVideo(page, 1);
    await expect.poll(() => streamsAt(page, '480×320')).toBe(0);
  });
});
