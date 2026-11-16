// The decoder's lifecycle in VideoStream (web/video-stream.js), with a stand-in for WebCodecs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { VideoStream, setDecoderStep } from '../../web/video-stream.js';
import { fletcher16, FLAG_KEYFRAME, VIDEO_HEADER_LEN } from '../../web/proto.js';

const decoders = [];
let supportWait = 0;
globalThis.VideoDecoder = class {
  constructor(init) {
    this.init = init;
    this.state = 'unconfigured';
    this.chunks = 0;
    decoders.push(this);
  }
  configure() { this.state = 'configured'; }
  decode() { this.chunks++; }
  close() { this.state = 'closed'; }
  static async isConfigSupported() {
    if (supportWait) await new Promise((r) => setTimeout(r, supportWait));
    return { supported: true };
  }
};
globalThis.EncodedVideoChunk = class {};

// A keyframe with an SPS, enough for ensureDecoder.
const KEY = new Uint8Array([0, 0, 0, 1, 0x67, 0x64, 0, 0x33, 0xac, 0, 0, 0, 1, 0x65, 0x88]);
const keyframe = () => ({ parts: [KEY], checksum: fletcher16(KEY), keyframe: true, hevc: false, captureUs: 1 });

function setup(extra = {}) {
  const log = { posts: [], logs: [], keyframes: 0, acks: [], firstFrames: 0 };
  const s = new VideoStream(0, {
    log: (level, msg) => log.logs.push(msg),
    post: (m) => log.posts.push(m),
    stats: { frames: 0, bytes: 0, dropped: 0, latencySamples: 0, latencySum: 0, latencyMax: 0 },
    hostNowUs: () => undefined,
    requestKeyframe: () => { log.keyframes++; },
    ack: (_, id) => log.acks.push(id),
    firstFrame: () => { log.firstFrames++; },
    ...extra,
  });
  return { s, log };
}

test('a decoder error is no connection problem: no status, and a keyframe brings it back', () => {
  setDecoderStep(0);
  const { s, log } = setup();
  s.decodeFrame(1, keyframe());
  decoders.at(-1).init.error(new Error('bad frame'));
  assert.deepEqual(log.posts.filter((m) => m.type === 'status'), []);
  assert.ok(log.logs.includes('decoder error detail'));
  assert.equal(s.decoder, undefined);
  assert.equal(s.waitingForKeyframe, true);
  s.decodeFrame(2, keyframe());
  assert.ok(s.decoder);
});

test('late word from a replaced decoder leaves the new one alone', () => {
  const { s, log } = setup();
  s.decodeFrame(1, keyframe());
  const old = decoders.at(-1);
  s.release();
  s.decodeFrame(2, keyframe());
  const current = s.decoder;
  assert.notEqual(current, old);
  old.init.error(new Error('queued before the swap'));
  assert.equal(s.decoder, current);
  // A picture from before it: closed, not drawn, and not this connection's first.
  let closed = false;
  old.init.output({ timestamp: 1, close() { closed = true; } });
  assert.ok(closed);
  assert.equal(log.firstFrames, 0);
});

test('the ladder takes one step when streams fail together', async () => {
  setDecoderStep(0);
  const a = setup();
  const b = setup();
  a.s.codec = b.s.codec = 'avc1.640033';
  supportWait = 5;
  await Promise.all([a.s.stepDownDecoder(), b.s.stepDownDecoder()]);
  supportWait = 0;
  assert.deepEqual([...a.log.posts, ...b.log.posts].filter((m) => m.type === 'decoderStep'), [{ type: 'decoderStep', step: 1 }]);
  setDecoderStep(0);
});

test('paused: what arrives is acknowledged but brings no decoder back, and asks for nothing', () => {
  let paused = false;
  const { s, log } = setup({ paused: () => paused });
  s.decodeFrame(1, keyframe());
  const made = decoders.length;
  s.release();
  paused = true;
  s.onFragment({ frameId: 9, fragIdx: 0, fragCount: 1, flags: FLAG_KEYFRAME, captureUs: 2, checksum: fletcher16(KEY) },
    new Uint8Array([...new Uint8Array(VIDEO_HEADER_LEN), ...KEY]));
  assert.equal(decoders.length, made);
  assert.equal(s.decoder, undefined);
  assert.deepEqual(log.acks, [9]);
  assert.equal(s.pending.size, 0);
  s.requestKeyframe();
  assert.equal(log.keyframes, 0);
  assert.equal(s.waitingForKeyframe, true);
});

test('the wait for a first picture counts from when it was asked for', () => {
  const { s, log } = setup({ nackOn: () => true, nack() {} });
  s.waitingSince = performance.now() - 5000; // the last session ended a while ago
  s.expectPicture();
  s.repair(0);
  assert.equal(log.keyframes, 0, 'too soon to ask');
  s.waitingSince = performance.now() - 1500;
  s.repair(0);
  assert.equal(log.keyframes, 1);
});
