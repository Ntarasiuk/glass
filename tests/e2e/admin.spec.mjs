// The owner's console at /admin/, and device posture, against real `glass --test` hosts.

import { Host, expectVideo, test, expect, toolbarClick } from './harness.mjs';

const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

test.describe('console', () => {
  it('the API is for the owner only', async ({ host }) => {
    expect((await fetch(`${host.base}/admin/api/state`)).status).toBe(403);
    const ok = await fetch(`${host.base}/admin/api/state`, { headers: { authorization: `Bearer ${host.token}` } });
    expect(ok.status).toBe(200);
    expect((await fetch(`${host.base}/admin/auth?t=wrong`, { redirect: 'manual' })).status).toBe(403);
  });

  it('shows viewers, makes a working view-only link, revokes it, changes settings and groups', async ({ browser, host }) => {
    // A viewer, so there's someone to see.
    const viewer = await (await browser.newContext()).newPage();
    await viewer.goto(host.url());
    await expectVideo(viewer);

    const ctx = await browser.newContext();
    const page = await ctx.newPage();
    await page.goto(`${host.base}/admin/auth?t=${host.token}`);
    await expect(page).toHaveURL(`${host.base}/admin/`);
    await expect(page.locator('#viewers li')).toHaveText(/Chrome on Mac/);

    // A view-only link: the guest gets video but no keyboard button.
    await page.locator('#link-form input[name=view_only]').check();
    await page.locator('#link-form button[type=submit]').click();
    await expect(page.locator('#made-summary')).toContainText('view only');
    await expect(page.locator('#qr svg')).toBeVisible();
    const url = await page.locator('#made-url').textContent();
    const guest = await (await browser.newContext()).newPage();
    await guest.goto(`${host.base}/#l=${url.match(/#l=([0-9a-f]{32})/)[1]}`);
    await expectVideo(guest);
    await expect(guest.locator('#btn-keyboard')).toBeHidden();

    // Revoke it from the console: the guest is cut off.
    await page.reload();
    await expect(page.locator('#passes li')).toHaveText(/view only/);
    await page.locator('#passes li button', { hasText: 'Revoke' }).click();
    await expect(guest.locator('#auth')).toBeVisible({ timeout: 20_000 });

    // A setting changed here is what `glass config` sees.
    await page.locator('#settings input[data-key="ask-guests"]').check();
    await expect.poll(() => host.cli('config', 'ask-guests').trim()).toBe('on');

    // Groups in the address book, which the viewer's machines menu also uses.
    await page.locator('#group-form input[name=name]').fill('Home');
    await page.locator('#group-form button').click();
    await expect(page.locator('#machines h3', { hasText: 'Home' })).toBeVisible();
    await page.locator('select[aria-label="Group for nas"]').selectOption('Home');
    await expect.poll(async () => (await (await fetch(`${host.base}/admin/api/groups`, { headers: { authorization: `Bearer ${host.token}` } })).json()).groups)
      .toEqual([{ name: 'Home', machines: ['nas.example.ts.net'] }]);
    // The offline machine with a known address can be woken.
    await page.locator('#machines li', { hasText: 'nas' }).locator('button', { hasText: 'Wake' }).click();
    await expect(page.locator('#toast')).toContainText('Wake-up sent');
    expect((await host.get('/test/records?kind=wake')).records).toContain('02:00:00:00:00:02');
  });

  it('the viewer links the owner to the console', async ({ page, host }) => {
    await page.goto(host.url());
    await expectVideo(page);
    await toolbarClick(page, 'btn-more');
    const [consolePage] = await Promise.all([
      page.context().waitForEvent('page'),
      page.locator('#more-panel button', { hasText: 'Console' }).click(),
    ]);
    await expect(consolePage).toHaveURL(/\/admin\/$/);
    await expect(consolePage.locator('#console')).toBeVisible();
  });
});

test.describe('device posture', () => {
  it('turns away a browser that is too old, with the reason, and lets it in once relaxed', async ({ browser, host }) => {
    host.cli('config', 'posture-min-browser', '999');
    const page = await (await browser.newContext()).newPage();
    await page.goto(host.url());
    await expect(page.locator('#status-title')).toHaveText('Not let in');
    await expect(page.locator('#status-detail')).toContainText('version 999 or newer');
    const log = JSON.parse(host.cli('log', '--json')).connections;
    expect(log.some((e) => e.how === 'refused' && e.detail.startsWith('posture:'))).toBe(true);

    host.cli('config', 'posture-min-browser', '0');
    await page.locator('.toast button', { hasText: 'Ask again' }).click();
    await expectVideo(page);
  });

  it('checks the operating system the browser reports', async ({ browser, host }) => {
    host.cli('config', 'posture-os', 'ios');
    const page = await (await browser.newContext()).newPage();
    await page.goto(host.url());
    await expect(page.locator('#status-detail')).toContainText('reported by the browser');
    host.cli('config', 'posture-os', '');
  });
});
