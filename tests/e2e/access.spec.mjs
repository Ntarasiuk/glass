// Scoped links, asking before letting someone in, chat, the connection log and the CLI
// socket, against real `glass --test` hosts.

import { Host, expectVideo, test, expect, toolbarClick } from './harness.mjs';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);

const it = test.extend({
  host: async ({}, use, testInfo) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
    if (testInfo.status !== testInfo.expectedStatus) {
      console.log(require('node:fs').readFileSync(`${host.dir}/logs/host.log`, 'utf8'));
    }
  },
});

/** A magic link to `host` with extra `glass link` flags; the host must have written its status. */
async function makeLink(host, ...flags) {
  for (let i = 0; i < 30; i++) {
    try {
      const made = JSON.parse(host.cli('link', '--json', '--for', '1h', ...flags));
      return { ...made, url: `${host.base}/#l=${made.url.match(/#l=([0-9a-f]{32})$/)[1]}` };
    } catch {
      await new Promise((r) => setTimeout(r, 200)); // status not written yet
    }
  }
  throw new Error('glass link never succeeded');
}

async function records(host, kind) {
  return (await host.get(`/test/records?kind=${kind}`)).records;
}

test.describe('scoped links', () => {
  it('a view-only link shows the screen but sends no input and hides file buttons', async ({ browser, host }) => {
    const made = await makeLink(host, '--view-only', '--no-files');
    expect(made.scope).toMatchObject({ view_only: true, files: false, clipboard: true });
    expect(made.summary).toContain('(view only, no files)');
    const guest = await (await browser.newContext()).newPage();
    await guest.goto(made.url);
    await expectVideo(guest);
    await expect(guest.locator('#btn-upload')).toBeHidden();
    await expect(guest.locator('#btn-download')).toBeHidden();
    await expect(guest.locator('#btn-keyboard')).toBeHidden();
    await host.clearInput();
    const canvas = guest.locator('#screens canvas').first();
    await canvas.click({ position: { x: 100, y: 100 } });
    await guest.keyboard.press('KeyA');
    await guest.waitForTimeout(500);
    expect((await host.input()).events).toEqual([]);
  });

  it('lists passes and revokes one device at a time', async ({ browser, host }) => {
    const a = await makeLink(host);
    const b = await makeLink(host, '--no-clipboard');
    const pa = await (await browser.newContext()).newPage();
    const pb = await (await browser.newContext()).newPage();
    await pa.goto(a.url);
    await pb.goto(b.url);
    await expectVideo(pa);
    await expectVideo(pb);
    const { passes } = JSON.parse(host.cli('link', '--list', '--json'));
    expect(passes).toHaveLength(2);
    const noClip = passes.find((p) => !p.scope.clipboard);
    expect(noClip.device).toMatch(/Chrome/);
    expect(JSON.parse(host.cli('link', '--revoke', noClip.id, '--json'))).toEqual({ revoked: 1 });
    await expect(pb.locator('#auth')).toBeVisible({ timeout: 20_000 });
    await pa.waitForTimeout(1000);
    await expect(pa.locator('#auth')).toBeHidden();
  });

  it('a link without the clipboard can\'t paste an image either', async ({ browser, host }) => {
    const made = await makeLink(host, '--no-clipboard');
    const guest = await (await browser.newContext()).newPage();
    await guest.goto(made.url);
    await expectVideo(guest);
    await guest.evaluate(() => {
      const data = new DataTransfer();
      data.setData('text/plain', 'from a guest');
      data.items.add(new File([new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])], 'x.png', { type: 'image/png' }));
      document.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    });
    // The host hears it and drops it.
    const hostLog = () => require('node:fs').readFileSync(`${host.dir}/logs/host.log`, 'utf8');
    await expect.poll(() => hostLog().includes('clipboard is off for this link')).toBe(true);
    expect(await host.get('/test/clipboard/rich')).toEqual({ text: '', html: null, image: null });
  });
});

test.describe('asking first', () => {
  it('waits for the owner to allow a guest, and stays out when denied', async ({ browser, host }) => {
    const made = await makeLink(host, '--ask');
    const guest = await (await browser.newContext()).newPage();
    await guest.goto(made.url);
    await expect(guest.locator('#status-title')).toHaveText('Waiting to be let in');
    await expect.poll(async () => (await host.get('/test/approvals')).pending.length).toBe(1);
    const { pending } = await host.get('/test/approvals');
    expect(pending[0].who).toMatch(/guest/);
    // Through the CLI socket, as an owner on SSH would.
    expect(host.cli('requests')).toContain(String(pending[0].id));
    host.cli('allow', String(pending[0].id));
    await expectVideo(guest);

    // Ask everyone, then deny: the viewer stops and offers to ask again.
    host.cli('config', 'ask-everyone', 'on');
    const owner = await (await browser.newContext()).newPage();
    await owner.goto(host.url());
    await expect.poll(async () => (await host.get('/test/approvals')).pending.length).toBe(1);
    host.cli('deny');
    await expect(owner.locator('#status-title')).toHaveText('Not let in');
    await owner.waitForTimeout(2500);
    expect((await host.get('/test/approvals')).pending).toEqual([]); // didn't knock again
    await owner.locator('.toast button', { hasText: 'Ask again' }).click();
    await expect.poll(async () => (await host.get('/test/approvals')).pending.length).toBe(1);
    await host.post('/test/approvals?allow=true');
    await expectVideo(owner);

    const log = JSON.parse(host.cli('log', '--json')).connections;
    expect(log.some((e) => e.how === 'refused' && e.detail === 'not allowed')).toBe(true);
  });
});

test.describe('chat', () => {
  it('messages reach the host, other viewers, and come back from glass chat', async ({ browser, host }) => {
    const a = await (await browser.newContext()).newPage();
    const b = await (await browser.newContext()).newPage();
    await a.goto(host.url());
    await b.goto(host.url());
    await expectVideo(a);
    await expectVideo(b);
    await toolbarClick(a, 'btn-chat');
    await a.locator('#chat-panel input').fill('hello from a');
    await a.locator('#chat-panel input').press('Enter');
    await expect(a.locator('#chat-panel li.mine')).toHaveText(/hello from a/);
    await expect(b.locator('.toast', { hasText: 'hello from a' })).toBeVisible();
    await expect(b.locator('#btn-chat .dot')).toBeVisible();
    expect((await records(host, 'notification')).some((n) => n.body === 'hello from a')).toBe(true);
    // The person at the computer answers from the CLI.
    expect(host.cli('chat', 'hi', 'there')).toContain('Sent to 2 viewer(s)');
    await expect(a.locator('#chat-panel li:not(.mine)')).toHaveText(/hi there/);
    // Typing in the chat never reaches the remote keyboard.
    await host.clearInput();
    await a.locator('#chat-panel input').fill('x');
    await a.waitForTimeout(300);
    expect((await host.input('keydown')).events).toEqual([]);
  });
});

test.describe('the host CLI', () => {
  it('lists and kicks viewers, and logs each session', async ({ page, host }) => {
    await page.goto(host.url());
    await expectVideo(page);
    const { viewers } = JSON.parse(host.cli('viewers', '--json'));
    expect(viewers).toHaveLength(1);
    expect(host.cli('kick', String(viewers[0].id))).toContain('Disconnected 1');
    await expect.poll(() => JSON.parse(host.cli('log', '--json')).connections.length).toBeGreaterThan(0);
    const [entry] = JSON.parse(host.cli('log', '--json')).connections;
    expect(entry).toMatchObject({ how: 'token', remote: false });
    expect(host.cli('log')).toContain('token');
  });

  it('settings round-trip through glass config', async ({ host }) => {
    host.cli('config', 'name', 'Studio Mac');
    expect(host.cli('config', 'name').trim()).toBe('Studio Mac');
    expect(host.cli('config')).toContain('ask-guests');
  });

  it('drops a viewer whose page went quiet but left its connection open', async ({ page, host }) => {
    test.setTimeout(120_000);
    await page.goto(host.url());
    await expectVideo(page);
    const list = () => JSON.parse(host.cli('viewers', '--json')).viewers;
    const viewers = () => list().length;
    expect(viewers()).toBe(1);
    const first = list()[0].id;
    // The host stops hearing the page's pings while the connection itself stays up, as
    // when Safari keeps a reloaded page's connection open. It ends that session (the page
    // may already be back with a new one by the time we look).
    await host.post('/test/deaf?on=true');
    await expect.poll(() => list().every((v) => v.id !== first), { timeout: 30_000, intervals: [500] }).toBe(true);
    await host.post('/test/deaf?on=false');
    // The page reconnects by itself (after its backoff).
    await expect.poll(viewers, { timeout: 60_000, intervals: [1000] }).toBe(1);
  });

  it('wakes a machine by MAC address', async ({ host }) => {
    expect(host.cli('wake', 'aa:bb:cc:dd:ee:ff')).toContain('aa:bb:cc:dd:ee:ff');
  });
});
