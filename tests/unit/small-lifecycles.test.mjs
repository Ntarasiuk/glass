// Gamepad buttons on focus loss, one stroke at a time, overlapping welcomes for
// notifications (web/gamepad.js, web/annotate.js, web/push.js).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Input } from '../../web/proto.js';
import { attachGamepads } from '../../web/gamepad.js';
import { createAnnotator } from '../../web/annotate.js';
import { createPush } from '../../web/push.js';

test('gamepad: what\'s held is released when the page loses focus, and sent again on return', () => {
  let focused = true;
  let frame;
  globalThis.addEventListener = () => {};
  globalThis.document = { hasFocus: () => focused };
  globalThis.requestAnimationFrame = (f) => { frame = f; return 1; };
  const pad = { index: 0, id: 'pad', buttons: [{ value: 1, pressed: true }], axes: [0.5] };
  Object.defineProperty(globalThis.navigator, 'getGamepads', { value: () => [pad], configurable: true });
  const sent = [];
  const pads = attachGamepads({ send: (kind, f) => sent.push([kind, f.keycode, f.a]) });
  pads.setEnabled(true);
  const poll = () => frame();
  poll();
  assert.deepEqual(sent.slice(1), [[Input.PAD_BUTTON, 0, 1], [Input.PAD_AXIS, 0, 0.5]]);
  sent.length = 0;
  focused = false;
  poll();
  assert.deepEqual(sent, [[Input.PAD_BUTTON, 0, 0], [Input.PAD_AXIS, 0, 0]]);
  sent.length = 0;
  poll();
  assert.deepEqual(sent, [], 'once');
  focused = true;
  poll();
  assert.deepEqual(sent, [[Input.PAD_BUTTON, 0, 1], [Input.PAD_AXIS, 0, 0.5]]);
});

function fakeElement() {
  const el = new EventTarget();
  Object.assign(el, {
    style: { setProperty() {} },
    classList: { add() {}, remove() {}, toggle() {} },
    setAttribute() {},
    append() {},
    after() {},
    setPointerCapture() {},
    querySelectorAll: () => [],
    getContext: () => ({ clearRect() {}, beginPath() {}, moveTo() {}, lineTo() {}, stroke() {} }),
  });
  return el;
}

test('drawing: a second finger neither joins the stroke nor ends it', () => {
  globalThis.document = { createElement: fakeElement, body: fakeElement() };
  globalThis.ResizeObserver = class { observe() {} };
  globalThis.innerWidth = globalThis.innerHeight = 100;
  globalThis.devicePixelRatio = 1;
  globalThis.requestAnimationFrame = () => 1;
  const picture = { dataset: { stream: '0' }, width: 100, height: 100, getBoundingClientRect: () => ({ left: 0, top: 0, width: 100, height: 100 }) };
  const screens = fakeElement();
  screens.querySelectorAll = () => [picture];
  let overlay;
  screens.after = (el) => { overlay = el; };
  const sent = [];
  const a = createAnnotator({ screens, send: (m) => sent.push(m) });
  a.setEnabled(true);
  const pointer = (type, pointerId, x) => overlay.dispatchEvent(Object.assign(new Event(type, { cancelable: true }), { pointerId, clientX: x, clientY: 50 }));
  pointer('pointerdown', 1, 10);
  pointer('pointerdown', 2, 90);
  pointer('pointermove', 2, 95);
  pointer('pointerup', 2, 95);
  pointer('pointermove', 1, 20);
  pointer('pointerup', 1, 20);
  const strokes = sent.filter((m) => m.t === 'annotate');
  assert.equal(new Set(strokes.map((m) => m.stroke.id)).size, 1);
  assert.deepEqual(strokes.flatMap((m) => m.stroke.points.map(([x]) => x)), [0.1, 0.2]);
  a.setEnabled(false);
});

test('notifications: an earlier host\'s welcome, answered late, leaves the later one\'s subscription', async () => {
  let release;
  const getting = new Promise((r) => { release = r; });
  let unsubscribed = 0;
  const sub = {
    endpoint: 'https://push/1',
    options: { applicationServerKey: Uint8Array.from([1, 2, 3]).buffer },
    toJSON: () => ({ endpoint: 'https://push/1', keys: { p256dh: 'p', auth: 'a' } }),
    unsubscribe: async () => { unsubscribed++; },
  };
  const registration = { pushManager: { getSubscription: async () => { await getting; return sub; } } };
  Object.defineProperty(globalThis.navigator, 'serviceWorker', { value: { register: async () => registration }, configurable: true });
  globalThis.window = { PushManager: class {}, Notification: class {} };
  globalThis.Notification = { permission: 'granted' };
  globalThis.matchMedia = () => ({ matches: false });
  const sent = [];
  const push = createPush({ control: (m) => sent.push(m), log() {}, toast() {}, url: 'https://x/' });
  // base64url of [1, 2, 3]: the key the subscription was made for.
  const stale = push.connected({ key: 'other-key', isOwner: true });
  const latest = push.connected({ key: 'AQID', isOwner: true });
  release();
  await Promise.all([stale, latest]);
  assert.equal(unsubscribed, 0);
  assert.equal(sent.filter((m) => m.t === 'pushSubscribe').length, 1);
  assert.equal(push.state, 'on');
});
