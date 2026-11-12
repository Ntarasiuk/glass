// Requests to the host whose answers carry no id, and the wake lock's slow answer.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { imageRequests } from '../../web/clip-image.js';
import { folderRequests, thumbRequests } from '../../web/files.js';
import { wakeHolder } from '../../web/device.js';

const pause = (ms = 1) => new Promise((r) => setTimeout(r, ms));

test('clipboard image: the newest copy waits for its own answer, not the one before', async () => {
  const ids = [];
  const images = imageRequests({ ask: (id) => ids.push(id), timeoutMs: 1000 });
  const first = images.next();
  const second = images.next();
  assert.equal(ids.length, 2);
  await assert.rejects(first, (e) => e.superseded);
  let got;
  second.then((v) => { got = v; });
  images.received('old', ids[0]);
  await pause();
  assert.equal(got, undefined);
  images.received('new', ids[1]);
  assert.equal(await second, 'new');
});

test('clipboard image: one that never comes times out, and its late answer isn\'t taken for the next', async () => {
  const ids = [];
  const images = imageRequests({ ask: (id) => ids.push(id), timeoutMs: 5 });
  await assert.rejects(images.next(), /never came/);
  const next = images.next();
  images.received('late', ids[0]);
  images.received('it', ids[1]);
  assert.equal(await next, 'it');
});

test('clipboard image: a host that echoes no id still gets its answer taken', async () => {
  const images = imageRequests({ ask() {}, timeoutMs: 1000 });
  const next = images.next();
  images.received('it');
  assert.equal(await next, 'it');
});

test('previews: requests lost with a session don\'t use up the slots', () => {
  const sent = [];
  const thumbs = thumbRequests((p) => sent.push(p));
  for (const p of ['a', 'b', 'c', 'd', 'e']) thumbs.ask(p);
  assert.deepEqual(sent, ['a', 'b', 'c', 'd']); // four at a time; none of these get answered
  thumbs.reset();
  thumbs.ask('f');
  assert.deepEqual(sent, ['a', 'b', 'c', 'd', 'f']);
  thumbs.answered('f', 'url');
  assert.equal(thumbs.get('f'), 'url');
  // Asked again after the reset: another computer's "a" isn't this one's.
  thumbs.ask('a');
  assert.equal(sent.at(-1), 'a');
});

test('previews: an answer frees a slot for the next; none is remembered as none', () => {
  const sent = [];
  const thumbs = thumbRequests((p) => sent.push(p));
  for (const p of ['a', 'b', 'c', 'd', 'e']) thumbs.ask(p);
  thumbs.answered('a', undefined);
  assert.equal(sent.at(-1), 'e');
  assert.equal(thumbs.get('a'), null);
  assert.equal(thumbs.get('e'), undefined);
});

test('folders: with ids, an old answer loses even when it names the folder wanted', () => {
  const f = folderRequests();
  const first = f.go('a');
  f.go('b');
  const third = f.go('a'); // back again: the first answer has the right path, but is stale
  assert.equal(f.reply('a', first), false);
  assert.equal(f.reply('tidied/a', third), true);
});

test('folders: answers out of order, the one asked for last wins', () => {
  const f = folderRequests();
  f.go('a');
  f.go('b');
  assert.equal(f.reply('b'), true);
  assert.equal(f.loading, true);
  assert.equal(f.reply('a'), false);
  assert.equal(f.loading, false);
});

test('folders: still loading until the last answer, an old one isn\'t shown meanwhile', () => {
  const f = folderRequests();
  f.go('a');
  f.go('b');
  assert.equal(f.reply('a'), false);
  assert.equal(f.loading, true);
  assert.equal(f.reply('b'), true);
  assert.equal(f.loading, false);
});

test('folders: a path the host tidied is still shown, and counts from then on', () => {
  const f = folderRequests();
  f.go('a/');
  assert.equal(f.reply('a'), true);
  f.changed();
  assert.equal(f.reply('a'), true);
});

test('folders: after a failed go, listings of the folder shown count again', () => {
  const f = folderRequests();
  f.go('a');
  f.reply('a');
  f.go('gone');
  f.reply('gone');
  f.stay('a');
  f.changed();
  assert.equal(f.reply('a'), true);
});

test('folders: a change answered while going elsewhere isn\'t shown', () => {
  const f = folderRequests();
  f.go('a');
  f.reply('a');
  f.changed();
  f.go('b');
  assert.equal(f.reply('a'), false);
  assert.equal(f.loading, true);
  assert.equal(f.reply('b'), true);
});

test('folders: a reset forgets what was on its way', () => {
  const f = folderRequests();
  f.go('a');
  f.reset();
  assert.equal(f.loading, false);
});

function lock() {
  const l = new EventTarget();
  l.released = false;
  l.release = async () => {
    l.released = true;
    l.dispatchEvent(new Event('release'));
  };
  return l;
}

test('wake lock: an answer after the session ended is let go', async () => {
  let wanted = true;
  let answer;
  const w = wakeHolder({ request: () => new Promise((r) => { answer = r; }), wanted: () => wanted, log() {} });
  const held = w.hold();
  wanted = false;
  w.release();
  const l = lock();
  answer(l);
  await held;
  assert.equal(l.released, true);
});

test('wake lock: one request at a time', async () => {
  let requests = 0;
  const w = wakeHolder({ request: async () => { requests++; return lock(); }, wanted: () => true, log() {} });
  await Promise.all([w.hold(), w.hold()]);
  await w.hold();
  assert.equal(requests, 1);
});

test('wake lock: an old lock\'s release doesn\'t forget a newer one', async () => {
  const locks = [];
  const w = wakeHolder({ request: async () => { const l = lock(); locks.push(l); return l; }, wanted: () => true, log() {} });
  await w.hold();
  const [old] = locks;
  w.release(); // its release event can come later than this
  await w.hold();
  old.dispatchEvent(new Event('release'));
  w.release();
  assert.equal(locks[1].released, true);
});

test('wake lock: a request that fails is logged, and can be tried again', async () => {
  const logged = [];
  let fail = true;
  const w = wakeHolder({
    request: async () => { if (fail) throw new Error('no'); return lock(); },
    wanted: () => true,
    log: (_, m) => logged.push(m),
  });
  await w.hold();
  assert.deepEqual(logged, ['wake lock: no']);
  fail = false;
  await w.hold();
  assert.equal(logged.length, 1);
});
