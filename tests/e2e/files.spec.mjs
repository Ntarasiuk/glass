// The file browser: browse a (temporary) home folder, upload into a folder, download,
// rename, make a folder and move to the Trash, against real `glass --test` hosts.

import { mkdirSync, mkdtempSync, readFileSync, existsSync, writeFileSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Host, expectVideo, test, expect, toolbarClick } from './harness.mjs';

const it = test.extend({
  home: async ({}, use) => {
    const home = mkdtempSync(join(tmpdir(), 'glass-files-'));
    mkdirSync(join(home, 'Documents/Taxes'), { recursive: true });
    mkdirSync(join(home, 'Downloads'));
    writeFileSync(join(home, 'Documents/notes.txt'), 'hello from the computer');
    writeFileSync(join(home, '.secret'), 'hidden');
    await use(home);
  },
  host: async ({ home }, use) => {
    const host = await new Host({ env: { GLASS_FILES_ROOT: home } }).start();
    await use(host);
    await host.stop();
  },
});

const sheet = (page) => page.locator('#files');
const item = (page, name) => page.locator('#files .item', { has: page.locator('.name', { hasText: new RegExp(`^${name}$`) }) });

it('browses, uploads into a folder, downloads, renames, makes a folder and trashes', async ({ page, host, home }) => {
  await page.goto(host.url());
  await expectVideo(page);
  await toolbarClick(page, 'btn-files');
  await expect(sheet(page)).toBeVisible();
  await expect(item(page, 'Documents')).toBeVisible();
  await expect(item(page, '.secret')).toHaveCount(0); // hidden files stay hidden

  // Into Documents (double-click), then filter.
  await item(page, 'Documents').dblclick();
  await expect(page.locator('#files .crumbs [aria-current]')).toHaveText('Documents');
  await expect(item(page, 'notes.txt')).toBeVisible();
  await expect(item(page, 'Taxes')).toBeVisible();
  await page.locator('#files .search input').fill('note');
  await expect(item(page, 'Taxes')).toHaveCount(0);
  await page.locator('#files .search input').fill('');

  // Upload into this folder.
  await page.locator('#files .picker').setInputFiles({ name: 'report.csv', mimeType: 'text/csv', buffer: Buffer.from('a,b\n1,2\n') });
  await expect.poll(() => existsSync(join(home, 'Documents/report.csv'))).toBe(true);
  expect(readFileSync(join(home, 'Documents/report.csv'), 'utf8')).toBe('a,b\n1,2\n');
  await expect(item(page, 'report.csv')).toBeVisible();

  // Download: from this host by URL.
  await item(page, 'notes.txt').click();
  const [download] = await Promise.all([page.waitForEvent('download'), page.locator('#files .open').click()]);
  expect(download.suggestedFilename()).toBe('notes.txt');
  expect(readFileSync(await download.path(), 'utf8')).toBe('hello from the computer');

  // Rename (F2), new folder, Trash.
  await item(page, 'report.csv').click();
  await page.keyboard.press('F2');
  await page.locator('#files .prompt input').fill('final.csv');
  await page.keyboard.press('Enter');
  await expect(item(page, 'final.csv')).toBeVisible();
  expect(existsSync(join(home, 'Documents/final.csv'))).toBe(true);

  await page.locator('#files .mkdir').click();
  await page.locator('#files .prompt input').fill('Receipts');
  await page.keyboard.press('Enter');
  await expect(item(page, 'Receipts')).toBeVisible();

  page.once('dialog', (d) => d.accept());
  await item(page, 'final.csv').click();
  await page.locator('#files .trash').click();
  await expect(item(page, 'final.csv')).toHaveCount(0);
  expect(readdirSync(join(home, '.Trash'))).toContain('final.csv');

  // Backspace goes up; the remote keyboard got none of this.
  await host.clearInput();
  await page.locator('#files .scroll').focus();
  await page.keyboard.press('Backspace');
  await expect(page.locator('#files .crumbs [aria-current]')).toHaveText('Home');
  expect((await host.input('keydown')).events).toEqual([]);
});

it('stays inside home: paths out and drag-out links without a ticket are refused', async ({ host }) => {
  const r = await fetch(`${host.base}/files/raw?t=nope&path=Documents/notes.txt`);
  expect(r.status).toBe(403);
});

it('is not offered to guests', async ({ browser, host }) => {
  let made;
  for (let i = 0; i < 30 && !made; i++) {
    try {
      made = JSON.parse(host.cli('link', '--json', '--for', '1h'));
    } catch {
      await new Promise((r) => setTimeout(r, 200));
    }
  }
  const guest = await (await browser.newContext()).newPage();
  await guest.goto(`${host.base}/#l=${made.url.match(/#l=([0-9a-f]{32})$/)[1]}`);
  await expectVideo(guest);
  await expect(guest.locator('#btn-upload')).toBeVisible();
  await expect(guest.locator('#btn-files')).toBeHidden();
});
