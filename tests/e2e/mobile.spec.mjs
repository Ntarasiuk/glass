// Phones: the toolbar's groups become dropdowns, and trackpad mode drives a cursor.

import { Host, expectVideo, test, expect } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

const phone = { viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, hasTouch: true, isMobile: true };

/** Fires touch pointer events on the screen, as a finger would. */
async function fingers(page, steps) {
  await page.evaluate(async (steps) => {
    const canvas = document.querySelector('#screens [data-stream]:not(.hidden)');
    const r = canvas.getBoundingClientRect();
    for (const s of steps) {
      if (s.wait) { await new Promise((ok) => setTimeout(ok, s.wait)); continue; }
      canvas.dispatchEvent(new PointerEvent(s.type, {
        bubbles: true, cancelable: true, pointerType: 'touch', pointerId: 10 + (s.id ?? 1), isPrimary: (s.id ?? 1) === 1,
        clientX: r.left + s.x, clientY: r.top + s.y,
      }));
    }
  }, steps);
}

it('the toolbar groups into dropdowns on a phone', async ({ browser, host }) => {
  const page = await (await browser.newContext(phone)).newPage();
  await page.goto(host.url());
  await expectVideo(page);
  // Groups show as one button each; their buttons are behind it.
  await expect(page.locator('#btn-upload')).toBeHidden();
  const files = page.locator('#tb-files .tb-trigger');
  await expect(files).toBeVisible();
  await files.click();
  await expect(page.locator('#btn-upload')).toBeVisible();
  await expect(page.locator('#tb-files')).toHaveClass(/open/);
  // Picking one closes the dropdown; tapping elsewhere does too.
  await page.locator('#tb-view .tb-trigger').click();
  await expect(page.locator('#tb-files')).not.toHaveClass(/open/);
  await page.locator('#btn-more').click();
  await expect(page.locator('#more-panel')).toBeVisible();
  await expect(page.locator('#tb-view')).not.toHaveClass(/open/);
  // The toolbar fits on the screen.
  const box = await page.locator('#toolbar').boundingBox();
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(390);
});

it('trackpad mode moves a cursor, clicks, right-clicks, drags and scrolls', async ({ browser, host }) => {
  const page = await (await browser.newContext(phone)).newPage();
  await page.goto(host.url());
  await expectVideo(page);
  // On by default on a phone, with its own cursor.
  await expect(page.locator('#btn-trackpad')).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('#tp-cursor')).toBeVisible();
  await host.clearInput();

  // A drag moves the cursor relative to where it was, not to the finger.
  await fingers(page, [{ type: 'pointerdown', x: 20, y: 20 }, { type: 'pointermove', x: 30, y: 20 }, { type: 'pointermove', x: 60, y: 20 }, { type: 'pointerup', x: 60, y: 20 }]);
  const moves = (await host.input('move')).events;
  expect(moves.length).toBeGreaterThan(0);
  expect(moves.at(-1).x).toBeGreaterThan(0.5); // started at the centre, moved right
  expect((await host.input('down')).events).toEqual([]); // moving isn't clicking

  // A tap clicks where the cursor is.
  await host.clearInput();
  await fingers(page, [{ type: 'pointerdown', x: 5, y: 5 }, { type: 'pointerup', x: 5, y: 5 }]);
  await expect.poll(async () => (await host.input('down')).events.map((e) => e.button)).toEqual([0]);
  expect((await host.input('move')).events.at(-1).x).toBeGreaterThan(0.5);

  // Two-finger tap: right click.
  await host.clearInput();
  await page.waitForTimeout(350); // not a double tap
  await fingers(page, [{ type: 'pointerdown', id: 1, x: 40, y: 40 }, { type: 'pointerdown', id: 2, x: 80, y: 40 }, { type: 'pointerup', id: 2, x: 80, y: 40 }, { type: 'pointerup', id: 1, x: 40, y: 40 }]);
  await expect.poll(async () => (await host.input('down')).events.map((e) => e.button)).toEqual([2]);

  // Double-tap and hold, then move: a drag.
  await host.clearInput();
  await page.waitForTimeout(350);
  await fingers(page, [
    { type: 'pointerdown', x: 50, y: 50 }, { type: 'pointerup', x: 50, y: 50 },
    { wait: 80 },
    { type: 'pointerdown', x: 50, y: 50 }, { type: 'pointermove', x: 70, y: 60 }, { type: 'pointermove', x: 90, y: 70 }, { type: 'pointerup', x: 90, y: 70 },
  ]);
  await expect.poll(async () => (await host.input()).events.filter((e) => e.t === 'down' || e.t === 'up').map((e) => `${e.t}${e.button}`)).toEqual(['down0', 'up0', 'down0', 'up0']);

  // Two fingers moving together scroll.
  await host.clearInput();
  await page.waitForTimeout(350);
  // (Both fingers in small steps, as real ones move together; big alternating jumps would
  // read as a pinch.)
  const steps = [];
  for (let y = 100; y >= 40; y -= 4) steps.push({ type: 'pointermove', id: 1, x: 40, y }, { type: 'pointermove', id: 2, x: 90, y });
  await fingers(page, [
    { type: 'pointerdown', id: 1, x: 40, y: 100 }, { type: 'pointerdown', id: 2, x: 90, y: 100 },
    ...steps,
    { type: 'pointerup', id: 2, x: 90, y: 40 }, { type: 'pointerup', id: 1, x: 40, y: 40 },
  ]);
  await expect.poll(async () => (await host.input('scroll')).events.length).toBeGreaterThan(0);
  expect((await host.input('down')).events).toEqual([]);

  // Off: touch is direct again (a tap clicks under the finger), and the cursor goes.
  await page.locator('#tb-input .tb-trigger').click();
  await page.locator('#btn-trackpad').click();
  await expect(page.locator('#tp-cursor')).toBeHidden();
});
