// A pen (Apple Pencil, stylus): pressure and tilt go ahead of the mouse events, and the pen
// leaving is reported, against a real `glass --test` host.

import { Host, expectVideo, test, expect } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

it('a pen stroke carries pressure and tilt, and its leaving', async ({ browser, host }) => {
  const page = await (await browser.newContext()).newPage();
  await page.goto(host.url());
  await expectVideo(page);
  await host.clearInput();
  await page.evaluate(() => {
    const canvas = document.querySelector('#screens canvas');
    const r = canvas.getBoundingClientRect();
    const at = (fx) => ({ clientX: r.left + r.width * fx, clientY: r.top + r.height / 2 });
    const pen = (type, fx, init) =>
      canvas.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: 7, pointerType: 'pen', ...at(fx), ...init }));
    const move = 'onpointerrawupdate' in window ? 'pointerrawupdate' : 'pointermove';
    pen(move, 0.2, { buttons: 0, pressure: 0, tiltX: 5 }); // hovering
    pen('pointerdown', 0.3, { button: 0, buttons: 1, pressure: 0.6, tiltX: 20, tiltY: -10, twist: 90 });
    pen(move, 0.4, { buttons: 1, pressure: 0.8, tiltX: 20, tiltY: -10, twist: 90 });
    pen('pointerup', 0.4, { button: 0, buttons: 0, pressure: 0, tiltX: 20, tiltY: -10, twist: 90 });
    pen('pointerleave', 0.4, { buttons: 0 });
  });
  await expect.poll(async () => (await host.input()).events.filter((e) => e.t !== 'move').map((e) => e.t)).toEqual([
    'pen', 'pen', 'down', 'pen', 'pen', 'up', 'pen',
  ]);
  const pens = (await host.input('pen')).events;
  expect(pens[0]).toMatchObject({ near: true, pressure: 0, tiltX: 5, tiltY: 0 });
  expect(pens[1]).toMatchObject({ near: true, tiltX: 20, tiltY: -10, twist: 90 });
  expect(pens[1].pressure).toBeCloseTo(0.6, 3);
  expect(pens[2].pressure).toBeCloseTo(0.8, 3);
  expect(pens[3]).toMatchObject({ near: true, pressure: 0 });
  expect(pens[4]).toMatchObject({ near: false });
  // Each pen sample still moves the pointer.
  expect((await host.input('move')).events.length).toBeGreaterThanOrEqual(4);
});

it('a mouse after the pen ends its proximity', async ({ browser, host }) => {
  const page = await (await browser.newContext()).newPage();
  await page.goto(host.url());
  await expectVideo(page);
  await host.clearInput();
  await page.evaluate(() => {
    const canvas = document.querySelector('#screens canvas');
    const move = 'onpointerrawupdate' in window ? 'pointerrawupdate' : 'pointermove';
    const ev = (pointerType, init) => canvas.dispatchEvent(new PointerEvent(move, { bubbles: true, pointerType, clientX: 50, clientY: 50, ...init }));
    ev('pen', { pointerId: 7, buttons: 0, pressure: 0 });
    ev('mouse', { pointerId: 1, buttons: 0 });
  });
  await expect.poll(async () => (await host.input('pen')).events.map((e) => e.near)).toEqual([true, false]);
});
