import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MAC_KEYCODES, charToKey } from '../../web/keymap.js';

test('every physical code maps to a unique Mac keycode', () => {
  const values = Object.values(MAC_KEYCODES);
  assert.equal(new Set(values).size, values.length);
  assert.equal(MAC_KEYCODES.KeyA, 0x00);
  assert.equal(MAC_KEYCODES.MetaLeft, 0x37);
  assert.equal(MAC_KEYCODES.Enter, 0x24);
});

test('typed characters map to keys with the right Shift state', () => {
  assert.deepEqual(charToKey('a'), { keycode: MAC_KEYCODES.KeyA, shift: false });
  assert.deepEqual(charToKey('A'), { keycode: MAC_KEYCODES.KeyA, shift: true });
  assert.deepEqual(charToKey('!'), { keycode: MAC_KEYCODES.Digit1, shift: true });
  assert.deepEqual(charToKey('/'), { keycode: MAC_KEYCODES.Slash, shift: false });
  assert.deepEqual(charToKey('?'), { keycode: MAC_KEYCODES.Slash, shift: true });
  assert.deepEqual(charToKey(' '), { keycode: MAC_KEYCODES.Space, shift: false });
});

test('iOS smart punctuation falls back to plain ASCII', () => {
  assert.deepEqual(charToKey('’'), charToKey("'"));
  assert.deepEqual(charToKey('“'), charToKey('"'));
  assert.deepEqual(charToKey('—'), charToKey('-'));
});

test('unmappable characters are skipped', () => {
  assert.equal(charToKey('é'), undefined);
  assert.equal(charToKey('😀'), undefined);
});
