import { test } from 'node:test';
import assert from 'node:assert/strict';
import { comboFromEvent, labelFor, keycodesFor, parseCustom, serializeCustom } from '../../web/shortcuts.js';
import { MAC_KEYCODES as K } from '../../web/keymap.js';

test('a keydown becomes a combo, modifiers in Mac menu order', () => {
  const combo = comboFromEvent({ code: 'Digit4', metaKey: true, shiftKey: true });
  assert.deepEqual(combo.codes, ['ShiftLeft', 'MetaLeft', 'Digit4']);
  assert.equal(combo.keysLabel, '⇧⌘4');
  assert.equal(comboFromEvent({ code: 'KeyQ', ctrlKey: true, metaKey: true }).keysLabel, '⌃⌘Q');
  assert.equal(comboFromEvent({ code: 'Space', altKey: true }).keysLabel, '⌥Space');
  assert.equal(comboFromEvent({ code: 'F5' }).keysLabel, 'F5');
});

test('modifiers alone, and keys the Mac lacks, are not combos yet', () => {
  assert.equal(comboFromEvent({ code: 'ShiftLeft', shiftKey: true }), null);
  assert.equal(comboFromEvent({ code: 'MetaRight', metaKey: true }), null);
  assert.equal(comboFromEvent({ code: 'NoSuchKey' }), null);
  assert.equal(comboFromEvent({}), null);
});

test('codes map to Mac keycodes, modifiers first', () => {
  assert.deepEqual(keycodesFor(['MetaLeft', 'ShiftLeft', 'Digit4']), [K.MetaLeft, K.ShiftLeft, K.Digit4]);
  assert.equal(labelFor(['MetaLeft', 'ArrowUp']), '⌘↑');
});

test('saved shortcuts round-trip and bad entries are dropped', () => {
  const saved = serializeCustom([{ label: 'Screenshot', codes: ['ShiftLeft', 'MetaLeft', 'Digit4'], keys: [1, 2, 3] }]);
  assert.equal(saved, '[{"label":"Screenshot","codes":["ShiftLeft","MetaLeft","Digit4"]}]');
  const [sc] = parseCustom(saved);
  assert.equal(sc.keysLabel, '⇧⌘4');
  assert.deepEqual(sc.keys, [K.ShiftLeft, K.MetaLeft, K.Digit4]);
  assert.deepEqual(parseCustom('not json'), []);
  assert.deepEqual(parseCustom('{"a":1}'), []);
  assert.deepEqual(parseCustom(null), []);
  assert.equal(parseCustom('[{"label":"x","codes":["Bogus"]},{"label":"y","codes":[]},{"codes":["KeyA"]}]').length, 0);
});
