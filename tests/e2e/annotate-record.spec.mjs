// Drawing on the screen (seen by every viewer) and recording sessions on the host.

import { mkdtempSync, readdirSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Host, expectVideo, test, expect, toolbarClick } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

async function open(page, host) {
  await page.goto(host.url());
  await expectVideo(page);
}

/** Opaque pixels on the drawing overlay. */
function inkOn(page) {
  return page.evaluate(() => {
    const c = document.getElementById('annotate');
    const data = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    let n = 0;
    for (let i = 3; i < data.length; i += 4) if (data[i] > 0) n++;
    return n;
  });
}

/** Drags across the middle of the first screen. */
async function scribble(page) {
  const box = await page.locator('#screens > [data-stream="0"]').boundingBox();
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + box.width * 0.3, y);
  await page.mouse.down();
  for (let i = 1; i <= 10; i++) await page.mouse.move(box.x + box.width * (0.3 + i * 0.03), y + i * 3);
  await page.mouse.up();
}

test.describe('drawing', () => {
  it('shows a stroke to the other viewer, and sends no input to the host', async ({ browser, host }) => {
    const a = await (await browser.newContext()).newPage();
    const b = await (await browser.newContext()).newPage();
    await open(a, host);
    await open(b, host);
    await host.clearInput();

    await toolbarClick(a, 'btn-draw');
    await expect(a.locator('#btn-draw')).toHaveAttribute('aria-pressed', 'true');
    await expect(a.locator('#draw-palette')).toBeVisible();
    await scribble(a);

    await expect.poll(() => inkOn(a)).toBeGreaterThan(50);
    await expect.poll(() => inkOn(b), { timeout: 5000 }).toBeGreaterThan(50);
    const { events } = await host.input();
    expect(events.filter((e) => e.t === 'down' || e.t === 'up')).toEqual([]);

    // Clearing removes it everywhere; strokes fade on their own too.
    await a.locator('#draw-palette button', { hasText: 'Clear' }).click();
    await expect.poll(() => inkOn(b), { timeout: 5000 }).toBe(0);
    await expect.poll(() => inkOn(a)).toBe(0);

    // Out of draw mode the pointer reaches the host again.
    await a.locator('#draw-palette button', { hasText: 'Done' }).click();
    await expect(a.locator('#draw-palette')).toBeHidden();
    await scribble(a);
    await expect.poll(async () => (await host.input('down')).events.length).toBeGreaterThan(0);
  });
});

test.describe('recording', () => {
  it('saves an MP4 on the host when the owner toggles it', async ({ page, host }) => {
    const dir = mkdtempSync(join(tmpdir(), 'glass-rec-'));
    host.cli('config', 'recordings-dir', dir);
    await open(page, host);
    const button = page.locator('#btn-record');
    await expect(button).toBeVisible();
    await toolbarClick(page, 'btn-record');
    await expect(button).toHaveAttribute('aria-pressed', 'true');
    await page.waitForTimeout(1500);
    await toolbarClick(page, 'btn-record');
    await expect(button).toHaveAttribute('aria-pressed', 'false');
    await expect(page.locator('.toast', { hasText: 'Recording saved' })).toBeVisible();

    const files = readdirSync(dir).filter((f) => f.endsWith('.mp4'));
    expect(files).toHaveLength(1);
    expect(files[0]).toMatch(/^Glass \d{4}-\d\d-\d\d \d\d-\d\d .+\.mp4$/);
    const mp4 = readFileSync(join(dir, files[0]));
    expect(mp4.subarray(4, 8).toString()).toBe('ftyp');
    expect(mp4.includes(Buffer.from('moov'))).toBe(true);
    expect(mp4.length).toBeGreaterThan(2000);
  });

  it('records every session when it is turned on in the settings', async ({ page, host }) => {
    const dir = mkdtempSync(join(tmpdir(), 'glass-rec-'));
    host.cli('config', 'recordings-dir', dir);
    host.cli('config', 'record', 'on');
    await open(page, host);
    await expect(page.locator('#btn-record')).toHaveAttribute('aria-pressed', 'true');
    await page.waitForTimeout(1000);
    await page.close();
    await expect.poll(() => readdirSync(dir).filter((f) => f.endsWith('.mp4')).length).toBe(1);
    host.cli('config', 'record', 'off');
  });
});
