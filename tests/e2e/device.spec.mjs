// "This device": notifications (Web Push, with a stand-in push service), one viewer tab per
// browser, and capturing the mouse, against real `glass --test` hosts.

import { Host, expectVideo, test, expect, toolbarClick } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

/**
 * Stands in for the browser's push service: notifications always allowed, and a
 * subscription with a real-looking endpoint and valid keys (the host checks both).
 */
async function fakePush(page) {
  await page.addInitScript(() => {
    const b64 = (bytes) => btoa(String.fromCharCode(...bytes)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
    // An uncompressed P-256 point (the RFC 8291 example's browser key) and an auth secret.
    const p256dh = 'BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4';
    let sub = null;
    const make = () => ({
      endpoint: 'https://fcm.googleapis.com/fcm/send/e2e-device',
      options: {},
      toJSON() { return { endpoint: this.endpoint, keys: { p256dh, auth: b64(new Uint8Array(16).fill(7)) } }; },
      unsubscribe: async () => { sub = null; return true; },
    });
    Object.defineProperty(Notification, 'permission', { get: () => 'granted' });
    Notification.requestPermission = async () => 'granted';
    PushManager.prototype.subscribe = async () => (sub = make());
    PushManager.prototype.getSubscription = async () => sub;
  });
}

async function pushes(host) {
  return (await host.get('/test/records?kind=push')).records;
}

/** The action key the page handed its service worker. */
function actionKey(page) {
  return page.evaluate(() => new Promise((resolve) => {
    const req = indexedDB.open('glass', 1);
    req.onsuccess = () => {
      const get = req.result.transaction('kv').objectStore('kv').get('pushKey');
      get.onsuccess = () => resolve(get.result);
    };
  }));
}

test.describe('notifications', () => {
  it('turn on from the device menu, then messages and connection requests arrive', async ({ browser, host }) => {
    const owner = await (await browser.newContext()).newPage();
    await fakePush(owner);
    await owner.goto(host.url());
    await expectVideo(owner);

    await toolbarClick(owner, 'btn-device');
    await owner.locator('#device-panel button', { hasText: 'Notifications' }).click();
    await expect(owner.locator('#device-panel button', { hasText: 'Questions' })).toBeVisible();
    await expect.poll(() => host.cli('notifications')).toMatch(/Chrome on Mac\s+messages, questions/);
    await expect.poll(() => actionKey(owner)).toBeTruthy();

    // A message from the person at the computer.
    host.cli('chat', 'back in five');
    await expect.poll(async () => (await pushes(host)).map((p) => p.payload.notification.body)).toContain('back in five');
    const chat = (await pushes(host)).find((p) => p.payload.notification.body === 'back in five');
    expect(chat.payload).toMatchObject({ web_push: 8030, app_badge: 1, notification: { navigate: `${host.base}/` } });

    // Someone asks to connect: Allow from the notification lets them in.
    host.cli('config', 'ask-everyone', 'on');
    const guest = await (await browser.newContext()).newPage();
    await guest.goto(host.url());
    await expect.poll(async () => (await pushes(host)).some((p) => p.payload.notification.data.answer?.what === 'connect')).toBe(true);
    const ask = (await pushes(host)).find((p) => p.payload.notification.data.answer?.what === 'connect');
    expect(ask.payload.notification.title).toBe('Someone wants to connect');

    const bad = await fetch(`${host.base}/push/act`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ key: 'nope', what: 'connect', id: ask.payload.notification.data.answer.id, yes: true }) });
    expect(bad.status).toBe(403);
    const key = await actionKey(owner);
    const good = await fetch(`${host.base}/push/act`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ key, what: 'connect', id: ask.payload.notification.data.answer.id, yes: true }) });
    expect(await good.json()).toEqual({ ok: true });
    await expectVideo(guest);

    // Off again: the host forgets this device.
    if (await owner.locator('#device-panel').isHidden()) await toolbarClick(owner, 'btn-device');
    await owner.locator('#device-panel button', { hasText: 'Notifications' }).click();
    await expect.poll(() => host.cli('notifications')).toMatch(/No devices/);
  });

  it('a subscription to anything but a push service is refused', async ({ browser, host }) => {
    const page = await (await browser.newContext()).newPage();
    await page.addInitScript(() => {
      Object.defineProperty(Notification, 'permission', { get: () => 'granted' });
      Notification.requestPermission = async () => 'granted';
      PushManager.prototype.getSubscription = async () => null;
      PushManager.prototype.subscribe = async () => ({
        endpoint: 'https://internal.example/steal',
        toJSON() { return { endpoint: this.endpoint, keys: { p256dh: 'BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4', auth: 'BwcHBwcHBwcHBwcHBwcHBw' } }; },
        unsubscribe: async () => true,
      });
    });
    await page.goto(host.url());
    await expectVideo(page);
    await toolbarClick(page, 'btn-device');
    await page.locator('#device-panel button', { hasText: 'Notifications' }).click();
    await expect(page.locator('.toast', { hasText: 'unknown push service' })).toBeVisible();
    expect(host.cli('notifications')).toMatch(/No devices/);
  });
});

test.describe('this device', () => {
  it('a second tab of the same computer asks before taking over', async ({ browser, host }) => {
    const ctx = await browser.newContext();
    const first = await ctx.newPage();
    await first.goto(host.url());
    await expectVideo(first);
    const second = await ctx.newPage();
    await second.goto(host.url());
    await expect(second.locator('#status-title')).toHaveText('Open in another tab');
    await second.locator('.toast button', { hasText: 'Use it here' }).click();
    await expectVideo(second);
    await expect(first.locator('#status-title')).toHaveText('Moved to another tab');
    await expect.poll(() => JSON.parse(host.cli('viewers', '--json')).viewers.length).toBe(1);
  });

  it('a captured mouse sends relative motion', async ({ browser, host }) => {
    const page = await (await browser.newContext()).newPage();
    await page.goto(host.url());
    await expectVideo(page);
    await host.clearInput();
    await page.evaluate(() => {
      const canvas = document.querySelector('#screens canvas');
      Object.defineProperty(document, 'pointerLockElement', { configurable: true, get: () => canvas });
      document.dispatchEvent(new Event('pointerlockchange'));
      for (const type of ['pointerrawupdate', 'pointermove']) {
        canvas.dispatchEvent(new PointerEvent(type, { bubbles: true, pointerType: 'mouse', movementX: 7, movementY: -3, clientX: 1, clientY: 1 }));
      }
    });
    await expect.poll(async () => (await host.input('moveBy')).events[0]).toMatchObject({ dx: 7, dy: -3 });
    expect((await host.input('move')).events).toEqual([]);
  });
});

test.describe('installed app', () => {
  it('still opens, and says so, when the computer is off', async ({ browser, host }) => {
    const page = await (await browser.newContext()).newPage();
    await page.goto(host.url());
    await expectVideo(page);
    await page.waitForFunction(() => navigator.serviceWorker.controller);
    await page.reload(); // now through the service worker, which keeps a copy
    await expectVideo(page);
    await host.stop();
    await page.reload();
    await expect(page.locator('#status-title')).toHaveText(/Can't reach/, { timeout: 15_000 });
    await expect(page.locator('#toolbar')).toBeVisible();
  });

  it('has a shortcut to each other machine, and opens one by short name', async ({ browser }) => {
    const other = await new Host().start();
    const machines = [
      { name: 'this-mac', dns: 'localhost', os: 'macOS', online: true, self: true, glass: true, wt_port: 0, macs: [] },
      { name: 'studio', dns: '127.0.0.1', os: 'macOS', online: true, self: false, glass: true, wt_port: other.wt, macs: [] },
      { name: 'printer', dns: 'printer.example', os: 'linux', online: true, self: false, glass: false, wt_port: null, macs: [] },
    ];
    const host = await new Host({ env: { GLASS_TEST_MACHINES: JSON.stringify(machines) } }).start();
    try {
      const manifest = await host.get('/manifest.webmanifest');
      expect(manifest.shortcuts.map((s) => s.name)).toEqual(['Chat', 'Machines', 'Send files', 'studio']);
      expect(manifest.shortcuts.at(-1).url).toBe(`./?machine=127.0.0.1:${other.wt}`);
      expect(manifest.protocol_handlers[0].protocol).toBe('web+glass');

      const page = await (await browser.newContext()).newPage();
      await page.goto(`${host.base}/?link=${encodeURIComponent('web+glass://studio')}#t=${host.token}`);
      await expect.poll(() => page.evaluate(() => localStorage.getItem('glass.machine'))).toBe(`127.0.0.1:${other.wt}`);
      expect(page.url()).not.toContain('link=');
    } finally {
      await host.stop();
      await other.stop();
    }
  });
});
