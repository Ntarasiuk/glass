// Custom shortcuts for the ⌘ menu: a key combo recorded in the page, kept in localStorage
// as KeyboardEvent.code names (layout-independent), sent as Mac keycodes.

import { MAC_KEYCODES } from './keymap.js';

export const CUSTOM_KEY = 'glass.customShortcuts';
const MAX_CUSTOM = 24;

// Held modifiers, in the order Mac menus show them.
const MODIFIERS = [
  ['ctrlKey', 'ControlLeft', '⌃'],
  ['altKey', 'AltLeft', '⌥'],
  ['shiftKey', 'ShiftLeft', '⇧'],
  ['metaKey', 'MetaLeft', '⌘'],
];
const MODIFIER_CODES = /^(Control|Alt|Shift|Meta|OS)(Left|Right)?$|^CapsLock$|^Fn$/;

const KEY_LABELS = {
  Space: 'Space', Tab: '⇥', Enter: '↩', Escape: '⎋', Backspace: '⌫', Delete: '⌦',
  ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→',
  PageUp: '⇞', PageDown: '⇟', Home: '↖', End: '↘',
  Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']', Backslash: '\\',
  Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/', Backquote: '`',
};

/** "⌘⇧4" for ['MetaLeft', 'ShiftLeft', 'Digit4']. */
export function labelFor(codes) {
  const mods = MODIFIERS.filter(([, code]) => codes.includes(code)).map(([, , sym]) => sym).join('');
  const key = codes.find((c) => !MODIFIER_CODES.test(c));
  return mods + (key ? keyLabel(key) : '');
}

function keyLabel(code) {
  if (KEY_LABELS[code]) return KEY_LABELS[code];
  const m = code.match(/^(?:Key|Digit|Numpad)(.+)$/);
  return m ? m[1] : code;
}

/**
 * The combo a keydown makes: modifiers held plus the key, or null while only modifiers
 * are down (wait for the real key) or for keys the Mac has no code for.
 */
export function comboFromEvent(e) {
  if (!e.code || MODIFIER_CODES.test(e.code) || MAC_KEYCODES[e.code] === undefined) return null;
  const codes = [...MODIFIERS.filter(([flag]) => e[flag]).map(([, code]) => code), e.code];
  return { codes, keysLabel: labelFor(codes) };
}

/** Mac keycodes to press, in order (modifiers first). */
export function keycodesFor(codes) {
  return codes.map((c) => MAC_KEYCODES[c]).filter((k) => k !== undefined);
}

/** Saved shortcuts, dropping anything malformed. */
export function parseCustom(text) {
  let list;
  try {
    list = JSON.parse(text ?? '[]');
  } catch {
    return [];
  }
  if (!Array.isArray(list)) return [];
  return list
    .filter((s) => s && typeof s.label === 'string' && Array.isArray(s.codes) && s.codes.length && s.codes.every((c) => typeof c === 'string' && MAC_KEYCODES[c] !== undefined))
    .slice(0, MAX_CUSTOM)
    .map((s) => ({ label: s.label.slice(0, 40), codes: s.codes, keysLabel: labelFor(s.codes), keys: keycodesFor(s.codes) }));
}

export function serializeCustom(list) {
  return JSON.stringify(list.slice(0, MAX_CUSTOM).map(({ label, codes }) => ({ label, codes })));
}
