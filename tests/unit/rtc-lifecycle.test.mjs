// Remote access's connection lifecycle: closing before it's up (web/rtc-transport.js and
// serveRtc in web/rtc.js), and the RTP numbers and fallback main.js goes by.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { RtcTransport } from '../../web/rtc-transport.js';
import { rtpPictureOverdue, rtpVideoStats, serveRtc } from '../../web/rtc.js';

const pause = (ms = 1) => new Promise((r) => setTimeout(r, ms));
async function until(check, what) {
  for (let i = 0; i < 300; i++) {
    if (check()) return;
    await pause();
  }
  assert.fail(`never happened: ${what}`);
}

function fakePort() {
  const sent = [];
  return { sent, postMessage: (m) => sent.push(m), onmessage: undefined };
}

test('closed before it was up, a transport never comes up, like WebTransport', async () => {
  const port = fakePort();
  const t = new RtcTransport(port);
  t.close();
  await assert.rejects(t.ready, /closed/);
  await assert.rejects(t.createBidirectionalStream(), /closed/);
  await assert.rejects(t.createUnidirectionalStream(), /closed/);
  // The answer to the connect it asked for comes too late to matter.
  port.onmessage({ data: { t: 'open', gen: t.gen } });
  await assert.rejects(t.ready, /closed/);
});

test('an open transport stays open until it ends', async () => {
  const port = fakePort();
  const t = new RtcTransport(port);
  port.onmessage({ data: { t: 'open', gen: t.gen } });
  await t.ready;
  await t.createUnidirectionalStream();
  t.close();
  await t.ready; // already settled: still resolved
});

test('a write waiting on a full stream fails when the connection ends, and so do later ones', async () => {
  const port = fakePort();
  const t = new RtcTransport(port);
  port.onmessage({ data: { t: 'open', gen: t.gen } });
  const writer = (await t.createUnidirectionalStream()).getWriter();
  await writer.write(new Uint8Array(1024 * 1024 + 1)); // past what may be in flight
  const blocked = writer.write(new Uint8Array(1));
  port.onmessage({ data: { t: 'closed', gen: t.gen, reason: 'gone' } });
  await assert.rejects(blocked, /connection closed/);
});

class FakeSocket {
  static made = [];
  static CLOSING = 2;
  constructor() {
    this.readyState = 1;
    this.sent = [];
    FakeSocket.made.push(this);
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
  send(m) { this.sent.push(m); }
  answer(msg) { this.onmessage({ data: JSON.stringify(msg) }); }
}

test('a close while waiting on the introducer ends the attempt: no peer connection, no open', async () => {
  globalThis.WebSocket = FakeSocket;
  let peers = 0;
  globalThis.RTCPeerConnection = class { constructor() { peers++; } };
  const port = fakePort();
  serveRtc({ port, hostKey: 'k', signalUrl: 'wss://x', log() {} });
  port.onmessage({ data: { t: 'connect', gen: 1 } });
  const ws = FakeSocket.made.at(-1);
  port.onmessage({ data: { t: 'close', gen: 1 } });
  assert.equal(ws.readyState, 3);
  await until(() => port.sent.some((m) => m.t === 'failed'), 'attempt ended');
  assert.equal(peers, 0);
  assert.ok(!port.sent.some((m) => m.t === 'open'));
});

test('a close for an older attempt leaves the current one alone', async () => {
  globalThis.WebSocket = FakeSocket;
  const port = fakePort();
  serveRtc({ port, hostKey: 'k', signalUrl: 'wss://x', log() {} });
  port.onmessage({ data: { t: 'connect', gen: 2 } });
  const ws = FakeSocket.made.at(-1);
  port.onmessage({ data: { t: 'close', gen: 1 } });
  assert.equal(ws.readyState, 1);
  port.onmessage({ data: { t: 'close', gen: 2 } });
});

const report = (framesDecoded, bytesReceived) => new Map([['v', { type: 'inbound-rtp', kind: 'video', framesDecoded, bytesReceived }]]);

test('RTP numbers: a new connection counts from zero, never negative', () => {
  const first = rtpVideoStats(report(100, 1000));
  assert.equal(first.stats.fps, 0);
  const second = rtpVideoStats(report(160, 2000), first.now);
  assert.equal(second.stats.fps, 60);
  assert.equal(second.stats.mbps, (1000 * 8) / 1e6);
  // Reconnected: the counters started over.
  const third = rtpVideoStats(report(5, 50), second.now);
  assert.equal(third.stats.fps, 0);
  assert.equal(third.stats.mbps, 0);
  assert.equal(rtpVideoStats(report(35, 100), third.now).stats.fps, 30);
});

test('no picture over RTP is only a fault for a signed-in viewer that\'s showing video', () => {
  const all = { rtp: true, live: true, showing: true, picture: false };
  assert.equal(rtpPictureOverdue(all), true);
  assert.equal(rtpPictureOverdue({ ...all, live: false }), false, 'at the sign-in screen, or waiting to be let in');
  assert.equal(rtpPictureOverdue({ ...all, showing: false }), false, 'hidden, or keyboard and mouse mode');
  assert.equal(rtpPictureOverdue({ ...all, picture: true }), false);
  assert.equal(rtpPictureOverdue({ ...all, rtp: false }), false);
});
