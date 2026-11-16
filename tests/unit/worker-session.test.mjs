// The worker's connection lifecycle (web/worker.js), against a fake WebTransport: what ends
// an attempt, what holds a viewer back, and that nothing reaches a host it has left.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { KIND_KEYFRAME_REQUEST } from '../../web/proto.js';

const realNow = performance.now.bind(performance);
let skew = 0;
Object.defineProperty(performance, 'now', { value: () => realNow() + skew, configurable: true });
globalThis.VideoDecoder = { isConfigSupported: async () => ({ supported: true }) };

const pause = (ms = 1) => new Promise((r) => setTimeout(r, ms));
async function until(check, what) {
  for (let i = 0; i < 2000; i++) {
    if (check()) return;
    await pause();
  }
  assert.fail(`never happened: ${what}`);
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

class FakeTransport {
  static made = [];
  constructor(url) {
    this.url = url;
    this.isClosed = false;
    this.sent = []; // control messages
    this.ready = new Promise((resolve, reject) => { this.open = resolve; this.refuse = reject; });
    this.ready.catch(() => {});
    this.closed = new Promise((resolve) => { this.resolveClosed = resolve; });
    this.datagrams = {
      readable: new ReadableStream({ start: (c) => { this.dgramIn = c; } }),
      writable: new WritableStream({ write: (d) => { this.datagramsOut.push(d); } }),
    };
    this.datagramsOut = [];
    this.incomingUnidirectionalStreams = new ReadableStream({ start: (c) => { this.incomingIn = c; } });
    FakeTransport.made.push(this);
  }
  /** The host drops the connection. */
  close() {
    if (this.isClosed) return;
    this.isClosed = true;
    this.refuse(new Error('transport closed'));
    this.resolveClosed();
    for (const c of [this.dgramIn, this.incomingIn, this.controlIn]) try { c?.close(); } catch {}
  }
  async createBidirectionalStream() {
    if (this.isClosed) throw new Error('transport closed');
    return {
      readable: new ReadableStream({ start: (c) => { this.controlIn = c; } }),
      writable: new WritableStream({
        write: (frame) => { this.sent.push(JSON.parse(decoder.decode(frame.subarray(4)))); },
      }),
    };
  }
  async createUnidirectionalStream() {
    if (this.isClosed) throw new Error('transport closed');
    return new WritableStream({ write() {} });
  }
  /** A control message from the host. */
  say(msg) {
    const body = encoder.encode(JSON.stringify(msg));
    const frame = new Uint8Array(4 + body.length);
    new DataView(frame.buffer).setUint32(0, body.length, true);
    frame.set(body, 4);
    this.controlIn.enqueue(frame);
  }
}
globalThis.WebTransport = FakeTransport;

let imports = 0;
/** A fresh worker, started on host A. Call `leave` at the end: it parks its reconnect loop. */
async function startWorker() {
  FakeTransport.made = [];
  skew = 0;
  const posted = [];
  const intervals = [];
  globalThis.setInterval = (fn, ms) => { intervals.push({ fn, ms }); return intervals.length; };
  const fakeSelf = { addEventListener() {}, postMessage: (m) => posted.push(m) };
  globalThis.self = fakeSelf;
  await import(`../../web/worker.js?instance=${++imports}`);
  const send = (data) => fakeSelf.onmessage({ data });
  send({ type: 'init', url: 'https://a/', token: 'tok' });
  await until(() => FakeTransport.made.length === 1, 'first attempt');
  return {
    send,
    posted,
    statuses: () => posted.filter((m) => m.type === 'status').map((m) => m.text),
    tick: () => intervals.find((i) => i.ms === 1000).fn(),
    repairTick: () => intervals.find((i) => i.ms === 20).fn(),
    transports: FakeTransport.made,
    leave: () => send({ type: 'leave' }),
  };
}
const hello = (t) => until(() => t.sent.some((m) => m.t === 'hello'), 'hello');

test('a stream the host stopped sending doesn\'t keep asking for keyframes', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  const canvas = { getContext: () => ({}) };
  w.send({ type: 'canvas', stream: 0, canvas });
  w.send({ type: 'canvas', stream: 1, canvas });
  a.open();
  await hello(a);
  a.say({ t: 'welcome', features: { nack: true } });
  await until(() => w.posted.some((m) => m.type === 'control' && m.msg.t === 'welcome'), 'welcome');
  w.send({ type: 'streams', count: 1 });
  skew += 5000;
  w.repairTick();
  await pause(5);
  const asked = a.datagramsOut.filter((d) => d[0] === KIND_KEYFRAME_REQUEST).map((d) => d[1]);
  assert.deepEqual(asked, [0]);
  w.leave();
});

test('switching machine during an attempt ends it: the old host hears nothing, the failure isn\'t counted', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  w.send({ type: 'switchHost', url: 'https://b/' });
  assert.equal(a.isClosed, true);
  await until(() => w.transports.length === 2, 'second attempt');
  assert.equal(w.transports[1].url, 'https://b/');
  assert.deepEqual(a.sent, []);
  // main.js counts "connection failed" against the machine it's now on.
  assert.ok(!w.statuses().some((s) => s.startsWith('connection failed')));
  w.leave();
});

test('a switch while the old host is answering doesn\'t sign in there', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  a.open();
  w.send({ type: 'switchHost', url: 'https://b/' }); // before the attempt has seen `ready`
  await until(() => w.transports.length === 2, 'second attempt');
  assert.deepEqual(a.sent, []);
  w.leave();
});

test('a switch is immediate even after failures have grown the retry delay', async () => {
  const w = await startWorker();
  w.transports[0].close();
  await until(() => w.transports.length === 2, 'retry');
  w.transports[1].close();
  await until(() => w.transports.length === 3, 'retry');
  w.send({ type: 'switchHost', url: 'https://b/' });
  await until(() => w.transports.length === 4, 'attempt at the new host');
  assert.equal(w.transports[3].url, 'https://b/');
  w.leave();
});

test('being turned away by one host doesn\'t stop a switch to another', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  a.open();
  await hello(a);
  a.say({ t: 'denied', msg: 'no' });
  await until(() => w.posted.some((m) => m.type === 'control' && m.msg.t === 'denied'), 'denied');
  a.close();
  await until(() => w.statuses().includes('not let in'), 'not let in');
  await pause(300);
  assert.equal(w.transports.length, 1, "doesn't knock again by itself");
  w.send({ type: 'switchHost', url: 'https://b/' });
  await until(() => w.transports.length === 2, 'attempt at the new host');
  assert.equal(w.transports[1].url, 'https://b/');
  w.leave();
});

test('a new token also ends a refusal', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  a.open();
  await hello(a);
  a.say({ t: 'error', msg: 'unauthorized' });
  await until(() => w.posted.some((m) => m.type === 'unauthorized'), 'unauthorized');
  a.close();
  await until(() => w.statuses().includes('waiting for access token'), 'waiting');
  w.send({ type: 'token', token: 'new' });
  await until(() => w.transports.length === 2, 'next attempt');
  w.transports[1].open();
  await hello(w.transports[1]);
  assert.equal(w.transports[1].sent[0].token, 'new');
  w.leave();
});

test('"Ask again" while another tab has the session doesn\'t take it back', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  a.open();
  await hello(a);
  a.say({ t: 'denied', msg: 'no' });
  await until(() => w.posted.some((m) => m.type === 'control' && m.msg.t === 'denied'), 'denied');
  a.close();
  await until(() => w.statuses().includes('not let in'), 'not let in');
  w.send({ type: 'suspend' });
  w.send({ type: 'askAgain' });
  await pause(50);
  assert.equal(w.transports.length, 1);
  w.send({ type: 'resume' });
  await until(() => w.transports.length === 2, 'attempt after resuming');
  w.leave();
});

test('what a left session still says is dropped', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  a.open();
  await hello(a);
  a.say({ t: 'denied', msg: 'from the old host' });
  w.send({ type: 'switchHost', url: 'https://b/' });
  await until(() => w.transports.length === 2, 'attempt at the new host');
  await pause(5);
  assert.ok(!w.posted.some((m) => m.type === 'control' && m.msg.t === 'denied'));
  assert.ok(!w.statuses().includes('not let in'));
  w.leave();
});

test('an attempt that hangs is given up on', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  w.tick();
  assert.equal(a.isClosed, false);
  skew += 21_000;
  w.tick();
  assert.equal(a.isClosed, true);
  await until(() => w.statuses().some((s) => s.startsWith('connection failed')), 'failure reported');
  await until(() => w.transports.length === 2, 'next attempt');
  w.leave();
});

test('a handshake hung after the transport came up is given up on too', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  a.createUnidirectionalStream = () => new Promise(() => {}); // never answers
  a.open();
  await hello(a);
  skew += 21_000;
  w.tick();
  assert.equal(a.isClosed, true);
  w.leave();
});

test('a file that can\'t be sent says so', async () => {
  const w = await startWorker();
  const file = new File(['hello'], 'a.txt');
  w.transports[0].close();
  await until(() => w.statuses().some((s) => s.startsWith('connection failed')), 'between attempts');
  w.send({ type: 'sendFile', file });
  await until(() => w.posted.some((m) => m.type === 'fileFailed'), 'failure');
  const failed = w.posted.find((m) => m.type === 'fileFailed');
  assert.equal(failed.name, 'a.txt');
  assert.equal(failed.direction, 'up');
  assert.match(failed.id, /a\.txt$/);
  w.leave();
});

test('a file cut off on its way says so', async () => {
  const w = await startWorker();
  const [a] = w.transports;
  a.open();
  await hello(a);
  a.createUnidirectionalStream = async () => new WritableStream({
    write() { throw new Error('lost'); },
  });
  w.send({ type: 'sendFile', file: new File(['hello'], 'b.txt') });
  await until(() => w.posted.some((m) => m.type === 'fileFailed'), 'failure');
  assert.equal(w.posted.find((m) => m.type === 'fileFailed').name, 'b.txt');
  assert.ok(!w.posted.some((m) => m.type === 'fileProgress' && m.done === m.total), 'never reported as sent');
  w.leave();
});

test('a stopped mic\'s worklet is told to stop, and a new one replaces it', async () => {
  const w = await startWorker();
  const told = [];
  const port = () => {
    const p = { postMessage: (m) => told.push([p, m]), close() { p.closed = true; } };
    return p;
  };
  const [first, second] = [port(), port()];
  w.send({ type: 'micPort', port: first });
  w.send({ type: 'micPort', port: second });
  assert.deepEqual(told, [[first, 'stop']]);
  assert.ok(first.closed);
  w.send({ type: 'mic', on: false });
  assert.deepEqual(told[1], [second, 'stop']);
  assert.ok(second.closed);
  w.leave();
});
