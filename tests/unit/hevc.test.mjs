// HEVC in the browser: codec strings from the SPS, and NAL parsing for both codecs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { findNal, hevcCodec, nalTypes } from '../../web/video-stream.js';

const START = [0, 0, 0, 1];

// A VideoToolbox-style Main profile SPS, level 4.1, with emulation-prevention bytes
// (00 00 03) inside the profile_tier_level, as real encoders write them.
const MAIN_SPS = [0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x7b, 0xa0, 0x03];

test('codec string from a Main profile SPS', () => {
  assert.equal(hevcCodec(Uint8Array.from(MAIN_SPS)), 'hvc1.1.6.L123.90');
});

test('codec string carries tier, profile and level', () => {
  // Main 10 (profile 2, compat bit 2), high tier, level 5.1 (153), constraints B0.
  const sps = [0x42, 0x01, 0x01, 0x22, 0x20, 0x00, 0x00, 0x00, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x99];
  assert.equal(hevcCodec(Uint8Array.from(sps)), 'hvc1.2.4.H153.B0');
});

test('finds the HEVC SPS among VPS, SPS, PPS and an IDR slice', () => {
  const vps = [0x40, 0x01, 0x0c];
  const pps = [0x44, 0x01, 0xc1];
  const idr = [0x26, 0x01, 0xaf];
  const buf = Uint8Array.from([...START, ...vps, ...START, ...MAIN_SPS, ...START, ...pps, ...START, ...idr]);
  assert.equal(nalTypes(buf, true), '32,33,34,19');
  assert.deepEqual(Array.from(findNal(buf, 33, true)), MAIN_SPS);
  // The same bytes read as H.264 would find no SPS (type 7).
  assert.equal(findNal(buf, 7), undefined);
});

test('H.264 parsing is unchanged', () => {
  const buf = Uint8Array.from([...START, 0x67, 0x64, 0x00, 0x33, ...START, 0x68, 0xee, ...START, 0x65, 0x88]);
  assert.equal(nalTypes(buf), '7,8,5');
  assert.deepEqual(Array.from(findNal(buf, 7)), [0x67, 0x64, 0x00, 0x33]);
});
