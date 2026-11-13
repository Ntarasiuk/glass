// The decoder ladder: a decoder chosen with ?decoder= is where it starts, but a fallback
// already found with that choice isn't undone on every load.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { probeDecoders, setDecoderStep } from '../../web/video-stream.js';

globalThis.VideoDecoder = { isConfigSupported: async () => ({ supported: true }) };

async function using(forced) {
  let picked;
  await probeDecoders((_level, _msg, data) => { picked = data.using; }, undefined, forced);
  return picked;
}

test('a pinned decoder starts there', async () => {
  setDecoderStep(0);
  assert.equal(await using('sw'), 'sw');
});

test('a fallback found with the pinned decoder is kept', async () => {
  // Pinned to hw, which failed and stepped down to sw (step 2) last session.
  setDecoderStep(2);
  assert.equal(await using('hw'), 'sw');
});

test('without a pin, the saved step is used', async () => {
  setDecoderStep(1);
  assert.equal(await using(null), 'hw');
});
