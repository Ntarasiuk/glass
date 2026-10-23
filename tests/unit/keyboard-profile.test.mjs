import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadProfile, saveProfile, defaultProfile } from '../../web/keyboard-profile.js';

function store() {
  const m = new Map();
  return { get: (k) => m.get(k) ?? null, set: (k, v) => m.set(k, v) };
}

test('each machine keeps its own remaps', () => {
  const s = store();
  saveProfile(s.set, 'mac.ts.net', { pc: true, ctrlAsCmd: false, swap: true });
  saveProfile(s.set, 'linux.ts.net', { pc: false, ctrlAsCmd: false, swap: false });
  assert.deepEqual(loadProfile(s.get, 'mac.ts.net'), { pc: true, ctrlAsCmd: false, swap: true, saved: true });
  assert.equal(loadProfile(s.get, 'linux.ts.net').pc, false);
});

test('a new Mac starts from the last settings; a new Linux host from its defaults', () => {
  const s = store();
  const fallback = { pc: true, ctrlAsCmd: true, swap: false };
  assert.deepEqual(loadProfile(s.get, 'new-mac', { fallback, hostOs: 'macos' }), { ...fallback, saved: false });
  assert.deepEqual(loadProfile(s.get, 'new-linux', { fallback, hostOs: 'linux', isPC: true }), { pc: false, ctrlAsCmd: false, swap: false, saved: false });
});

test('defaults and junk', () => {
  assert.deepEqual(defaultProfile({ hostOs: 'macos', isPC: true }), { pc: true, ctrlAsCmd: true, swap: false });
  const s = store();
  s.set('glass.keyboard.x', '{not json');
  assert.equal(loadProfile(s.get, 'x').saved, false);
});
