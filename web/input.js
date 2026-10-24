// Pointer, touch, keyboard and clipboard input → remote input events.

import { Input } from './proto.js';
import { MAC_KEYCODES, charToKey, KVK_SHIFT } from './keymap.js';

const LONG_PRESS_MS = 500;
const TAP_SLOP_PX = 10;
// A single touch waits this long before pressing, so a second finger can turn
// the gesture into a scroll without a stray click.
const TOUCH_PRESS_DELAY_MS = 120;
const CLIPBOARD_WAIT_MS = 1500;
const MAX_ZOOM = 5;
// A two-finger gesture becomes a pinch once the finger spread changes this much.
const PINCH_THRESHOLD = 0.08;
const PASTE_FALLBACK_MS = 150;

const MODIFIER_CODES = ['MetaLeft', 'MetaRight', 'ShiftLeft', 'ShiftRight', 'AltLeft', 'AltRight', 'ControlLeft', 'ControlRight'];
const MODIFIER_KEYCODES = new Set(MODIFIER_CODES.map((c) => MAC_KEYCODES[c]));

/**
 * @param canvas      the video canvas
 * @param keySink     hidden textarea that holds keyboard focus (and summons the iPad keyboard)
 * @param send        (kind, fields) => void, sends one input event
 * @param control     (msg) => void, sends a control message
 * @param nextClipboard () => Promise<string>, resolves with the next Mac clipboard text
 * @param onLocalShortcut (code) => void, for Ctrl+Shift+<key> shortcuts handled locally
 */
export function attachInput({
  screens, keySink, send, control, nextClipboard, onLocalShortcut, swapCmdOption = false, ctrlAsCmd = true,
  pcKeyboard = false, onZoom,
}) {
  // With iPadOS set to turn the physical ⌘ key into ⌥ (so it can't claim ⌘Tab etc.),
  // swap them back here so the Mac gets the keys the user actually pressed.
  let swap = swapCmdOption;
  const SWAPPED = {
    MetaLeft: 'AltLeft', MetaRight: 'AltRight', AltLeft: 'MetaLeft', AltRight: 'MetaRight',
  };
  // iPadOS claims these ⌘ shortcuts before any page sees them, so Ctrl+<key> stands in:
  // the Mac receives ⌘ for the rest of that chord (holding Ctrl and tapping Tab cycles apps).
  let remapCtrl = ctrlAsCmd;
  let remappedCtrl; // the Ctrl keycode currently standing in for ⌘, if any
  const CMD = MAC_KEYCODES.MetaLeft;
  const CTRL_KEYCODES = [MAC_KEYCODES.ControlLeft, MAC_KEYCODES.ControlRight];
  const stolenByIPad = (code, shift) =>
    code === 'Tab' || code === 'Space' || (code === 'KeyH' && !shift) ||
    (shift && ['Digit3', 'Digit4', 'Digit5'].includes(code));

  // PC keyboards (Linux, Windows): Ctrl is the everyday shortcut key, so it becomes ⌘ on
  // the Mac, and Super/Windows becomes Ctrl. Alt stays ⌥.
  let pc = pcKeyboard;
  const PC = {
    ControlLeft: 'MetaLeft', ControlRight: 'MetaRight', MetaLeft: 'ControlLeft', MetaRight: 'ControlRight',
  };

  // Remaps exist to make a non-Mac keyboard behave on a Mac; other hosts get keys as typed.
  let macHost = true;
  const codeOf = (e) => (!macHost ? undefined : pc ? PC[e.code] : swap ? SWAPPED[e.code] : undefined) ?? e.code;
  const cmdHeld = (e) => (!macHost ? e.metaKey : pc ? e.ctrlKey : swap ? e.altKey : e.metaKey);
  // Each canvas shows one host stream (data-stream) and is letterboxed by
  // `object-fit: contain`; pointer positions map into the video's content box. Rects are
  // cached and dropped on resize, so pointer events never force a layout read.
  const videoSizes = []; // stream -> { width, height }
  const rects = new Map(); // canvas -> content rect
  const resize = new ResizeObserver(() => rects.clear());
  const streamOf = (canvas) => Number(canvas.dataset.stream);
  // A display is a canvas (WebTransport) or a <video> (remote, over WebRTC).
  const canvasOf = (e) => e.target?.closest?.('#screens > [data-stream]') ?? undefined;

  function contentRect(canvas) {
    let rect = rects.get(canvas);
    const size = videoSizes[streamOf(canvas)];
    if (rect || !size) return rect;
    const r = canvas.getBoundingClientRect();
    const scale = Math.min(r.width / size.width, r.height / size.height);
    const w = size.width * scale;
    const h = size.height * scale;
    rect = { left: r.left + (r.width - w) / 2, top: r.top + (r.height - h) / 2, width: w, height: h };
    rects.set(canvas, rect);
    return rect;
  }

  // Pointer lock (Capture mouse): relative motion only, the local position means nothing.
  let locked = false;

  function sendMove(x, y, canvas) {
    if (locked) return;
    const rect = canvas && contentRect(canvas);
    if (!rect) return;
    send(Input.MOVE, { button: streamOf(canvas), a: (x - rect.left) / rect.width, b: (y - rect.top) / rect.height });
  }

  function focusKeys() {
    keySink.focus({ preventScroll: true });
  }

  // ---- Mouse, trackpad and pen: direct mapping ----

  const moveEvent = 'onpointerrawupdate' in window ? 'pointerrawupdate' : 'pointermove';
  screens.addEventListener(moveEvent, (e) => {
    if (locked) {
      if (e.movementX || e.movementY) send(Input.MOVE_BY, { a: e.movementX, b: e.movementY });
      return;
    }
    if (e.pointerType !== 'touch') sendMove(e.clientX, e.clientY, canvasOf(e));
  }, { passive: true });

  screens.addEventListener('pointerdown', (e) => {
    const canvas = canvasOf(e);
    if (!canvas) return;
    e.preventDefault();
    if (e.pointerType === 'touch') return touchDown(e, canvas);
    // Don't focus the key sink here: iPad Safari leaves fullscreen when a text field takes
    // focus, and hardware keys reach the window without it.
    if (!locked) try { canvas.setPointerCapture(e.pointerId); } catch {}
    sendMove(e.clientX, e.clientY, canvas);
    send(Input.DOWN, { button: e.button });
  });
  screens.addEventListener('pointerup', (e) => {
    e.preventDefault();
    if (e.pointerType === 'touch') return touchUp(e);
    sendMove(e.clientX, e.clientY, canvasOf(e));
    send(Input.UP, { button: e.button });
  });
  screens.addEventListener('pointercancel', (e) => {
    if (e.pointerType === 'touch') touchUp(e, true);
  });
  screens.addEventListener('contextmenu', (e) => e.preventDefault());
  screens.addEventListener('wheel', (e) => {
    const unit = e.deltaMode === WheelEvent.DOM_DELTA_LINE ? 16 : e.deltaMode === WheelEvent.DOM_DELTA_PAGE ? 400 : 1;
    // Point at the screen being scrolled first, so the scroll lands on that display.
    sendMove(e.clientX, e.clientY, canvasOf(e));
    send(Input.SCROLL, { a: e.deltaX * unit, b: e.deltaY * unit });
    e.preventDefault();
  }, { passive: false });

  // ---- Touch: tap = click, drag = drag, long-press = right-click, two fingers = scroll ----

  const touches = new Map(); // pointerId -> { x, y }
  let gesture; // { startX, startY, pressed, scrolling, done, pressTimer, longTimer }

  function centroid() {
    let x = 0;
    let y = 0;
    for (const t of touches.values()) { x += t.x; y += t.y; }
    return { x: x / touches.size, y: y / touches.size };
  }

  function clearTimers() {
    clearTimeout(gesture?.pressTimer);
    clearTimeout(gesture?.longTimer);
  }

  function touchDown(e, canvas) {
    // Capture can throw (e.g. a pointer the browser already considers gone); tracking the
    // gesture matters more than capture.
    try { canvas.setPointerCapture(e.pointerId); } catch {}
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (touches.size === 1) {
      sendMove(e.clientX, e.clientY, canvas);
      gesture = { canvas, startX: e.clientX, startY: e.clientY, pressed: false, scrolling: false, done: false };
      gesture.pressTimer = setTimeout(() => {
        if (!gesture.scrolling && !gesture.done && touches.size === 1) {
          gesture.pressed = true;
          send(Input.DOWN, { button: 0 });
        }
      }, TOUCH_PRESS_DELAY_MS);
      gesture.longTimer = setTimeout(() => {
        if (gesture.scrolling || gesture.done || touches.size !== 1) return;
        // Long press without moving: right-click instead of the pending left press.
        if (gesture.pressed) send(Input.UP, { button: 0 });
        send(Input.DOWN, { button: 2 });
        send(Input.UP, { button: 2 });
        gesture.done = true;
      }, LONG_PRESS_MS);
    } else if (gesture && !gesture.done) {
      clearTimers();
      if (gesture.pressed) send(Input.UP, { button: 0 });
      gesture.pressed = false;
      gesture.scrolling = true;
      gesture.last = centroid();
      // Decided on first movement: pinch (zoom), pan (when zoomed) or scroll the remote.
      gesture.mode = undefined;
      gesture.start = { c: centroid(), spread: spread(), zoom: { ...zoom } };
    }
  }

  // ---- Local zoom (pinch): scales the remote screens on this device only ----

  const zoom = { s: 1, x: 0, y: 0 };

  function spread() {
    const [a, b] = [...touches.values()];
    return b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  }

  /** Scale `s` around the fingers: the point that was under `c0` ends up under `c`. */
  function zoomTo(s, c, start) {
    s = Math.min(MAX_ZOOM, Math.max(1, s));
    const px = (start.c.x - start.zoom.x) / start.zoom.s;
    const py = (start.c.y - start.zoom.y) / start.zoom.s;
    const w = screens.clientWidth;
    const h = screens.clientHeight;
    // Keep the screens covering the viewport: no panning past the edges.
    zoom.s = s;
    zoom.x = Math.min(0, Math.max(w - w * s, c.x - px * s));
    zoom.y = Math.min(0, Math.max(h - h * s, c.y - py * s));
    applyZoom();
  }

  function applyZoom() {
    screens.style.transform = zoom.s === 1 ? '' : `translate(${zoom.x}px, ${zoom.y}px) scale(${zoom.s})`;
    rects.clear();
    onZoom?.(zoom.s);
  }

  function resetZoom() {
    Object.assign(zoom, { s: 1, x: 0, y: 0 });
    applyZoom();
  }

  screens.addEventListener('pointermove', (e) => {
    if (e.pointerType !== 'touch' || !touches.has(e.pointerId) || !gesture) return;
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (gesture.scrolling) {
      const c = centroid();
      const start = gesture.start;
      if (!gesture.mode && start && touches.size >= 2) {
        const ratio = spread() / (start.spread || 1);
        const moved = Math.hypot(c.x - start.c.x, c.y - start.c.y) > TAP_SLOP_PX;
        if (Math.abs(ratio - 1) > PINCH_THRESHOLD) gesture.mode = 'zoom';
        else if (moved) gesture.mode = zoom.s > 1 ? 'zoom' : 'scroll';
      }
      if (gesture.mode === 'zoom' && touches.size >= 2) {
        zoomTo(start.zoom.s * (spread() / (start.spread || 1)), c, start);
      } else if (gesture.mode === 'scroll') {
        // Natural scrolling: content follows the fingers.
        send(Input.SCROLL, { a: gesture.last.x - c.x, b: gesture.last.y - c.y });
      }
      gesture.last = c;
      return;
    }
    if (gesture.done) return;
    const moved = Math.hypot(e.clientX - gesture.startX, e.clientY - gesture.startY) > TAP_SLOP_PX;
    if (moved && !gesture.pressed) {
      // Started dragging before the press delay elapsed: press now.
      clearTimers();
      gesture.pressed = true;
      send(Input.DOWN, { button: 0 });
    } else if (moved) {
      clearTimeout(gesture.longTimer);
    }
    sendMove(e.clientX, e.clientY, gesture.canvas);
  }, { passive: true });

  function touchUp(e, cancelled = false) {
    touches.delete(e.pointerId);
    // Snap back to 1× when a pinch ends just above it.
    if (touches.size === 0 && gesture?.mode === 'zoom' && zoom.s < 1.05) resetZoom();
    if (!gesture || touches.size > 0) return;
    clearTimers();
    if (!gesture.done && !gesture.scrolling && !cancelled) {
      if (!gesture.pressed) send(Input.DOWN, { button: 0 });
      send(Input.UP, { button: 0 });
    } else if (gesture.pressed && !gesture.done) {
      send(Input.UP, { button: 0 });
    }
    gesture = undefined;
  }

  // ---- Keyboard ----

  const heldKeys = new Set();
  let pasteHandled = true;
  function releaseKeys(filter = () => true) {
    for (const keycode of heldKeys) {
      if (filter(keycode)) {
        send(Input.KEY_UP, { keycode });
        heldKeys.delete(keycode);
      }
    }
  }

  function tapKey(keycode, shift) {
    if (shift) send(Input.KEY_DOWN, { keycode: KVK_SHIFT });
    send(Input.KEY_DOWN, { keycode });
    send(Input.KEY_UP, { keycode });
    if (shift) send(Input.KEY_UP, { keycode: KVK_SHIFT });
  }

  // Typing in Glass's own fields (token prompt, share link) stays local.
  const localField = (el) => el !== keySink && el?.matches?.('input, textarea');

  window.addEventListener('keydown', (e) => {
    if (localField(e.target)) return;
    if (e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey && ['KeyF', 'KeyH'].includes(e.code)) {
      e.preventDefault();
      onLocalShortcut(e.code);
      return;
    }
    if (e.isComposing) return;
    const code = codeOf(e);
    const keycode = MAC_KEYCODES[code];
    // On-screen keyboards often report no code; their text arrives via beforeinput.
    if (keycode === undefined) return;

    if (cmdHeld(e) && code === 'KeyV' && !swap) {
      // Let the browser fire `paste`; the host gets the text and presses Cmd+V itself, so
      // the Mac pastes what's on this device's clipboard. Without a focused text field some
      // browsers never fire it, so fall back to sending ⌘V as a plain keystroke.
      pasteHandled = false;
      setTimeout(() => {
        if (pasteHandled) return;
        send(Input.KEY_DOWN, { keycode });
        send(Input.KEY_UP, { keycode });
      }, PASTE_FALLBACK_MS);
      return;
    }
    if (cmdHeld(e) && (code === 'KeyC' || code === 'KeyX') && navigator.clipboard?.write) {
      // Must be called during the keydown for Safari; the item resolves once the
      // Mac reports its new clipboard.
      const text = nextClipboard(CLIPBOARD_WAIT_MS).then((t) => new Blob([t], { type: 'text/plain' }));
      navigator.clipboard.write([new ClipboardItem({ 'text/plain': text })]).catch(() => {});
    }

    e.preventDefault();
    if (macHost && remapCtrl && !pc && !remappedCtrl && e.ctrlKey && !cmdHeld(e) && stolenByIPad(code, e.shiftKey)) {
      // Swap the Ctrl the Mac already sees for ⌘, until Ctrl is released.
      for (const ctrl of CTRL_KEYCODES) {
        if (heldKeys.delete(ctrl)) {
          send(Input.KEY_UP, { keycode: ctrl });
          remappedCtrl = ctrl;
        }
      }
      heldKeys.add(CMD);
      send(Input.KEY_DOWN, { keycode: CMD });
    }
    heldKeys.add(keycode);
    send(Input.KEY_DOWN, { keycode });
  });

  window.addEventListener('keyup', (e) => {
    if (localField(e.target)) return;
    const code = codeOf(e);
    const keycode = MAC_KEYCODES[code];
    if (keycode === undefined) return;
    e.preventDefault();
    if (keycode === remappedCtrl) {
      remappedCtrl = undefined;
      if (heldKeys.delete(CMD)) send(Input.KEY_UP, { keycode: CMD });
      return;
    }
    if (!heldKeys.delete(keycode)) return;
    send(Input.KEY_UP, { keycode });
    // macOS browsers swallow keyup for keys pressed while Cmd was held.
    if (code === 'MetaLeft' || code === 'MetaRight') releaseKeys((k) => !MODIFIER_KEYCODES.has(k));
  });

  window.addEventListener('blur', () => {
    releaseKeys();
    remappedCtrl = undefined;
  });

  document.addEventListener('paste', (e) => {
    if (localField(e.target)) return;
    e.preventDefault();
    pasteHandled = true;
    const text = e.clipboardData?.getData('text/plain');
    if (text) control({ t: 'clipboard', text, paste: true });
  });

  // On-screen keyboard text (and anything else that bypassed keydown).
  keySink.addEventListener('beforeinput', (e) => {
    e.preventDefault();
    switch (e.inputType) {
      case 'insertText':
      case 'insertReplacementText':
        for (const ch of e.data ?? '') {
          const key = charToKey(ch);
          if (key) tapKey(key.keycode, key.shift);
        }
        break;
      case 'insertLineBreak':
      case 'insertParagraph':
        tapKey(MAC_KEYCODES.Enter, false);
        break;
      case 'deleteContentBackward':
        tapKey(MAC_KEYCODES.Backspace, false);
        break;
      case 'deleteContentForward':
        tapKey(MAC_KEYCODES.Delete, false);
        break;
    }
  });

  return {
    setPointerLocked(on) {
      locked = on;
    },
    /** Presses keys in order and releases them in reverse, e.g. [Cmd, Tab]. */
    sendCombo(keycodes) {
      for (const keycode of keycodes) send(Input.KEY_DOWN, { keycode });
      for (const keycode of [...keycodes].reverse()) send(Input.KEY_UP, { keycode });
    },
    setVideoSize(stream, width, height) {
      videoSizes[stream] = { width, height };
      rects.clear();
    },
    observeCanvas(canvas) {
      resize.observe(canvas);
    },
    focusKeys,
    resetZoom,
    setCtrlAsCmd(on) {
      releaseKeys();
      remappedCtrl = undefined;
      remapCtrl = on;
    },
    setHostOs(os) {
      releaseKeys();
      remappedCtrl = undefined;
      macHost = os === 'macos';
    },
    setPcKeyboard(on) {
      releaseKeys();
      remappedCtrl = undefined;
      pc = on;
    },
    setSwapCmdOption(on) {
      releaseKeys();
      swap = on;
    },
  };
}
