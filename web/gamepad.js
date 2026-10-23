// Game controllers: polls the Gamepad API once per animation frame while a controller is
// connected and this page has focus, and sends only what changed. Uses the standard
// mapping (Xbox layout), which the host turns into a virtual controller.

import { Input } from './proto.js';

const MAX_PADS = 4;
// Stick noise below this is ignored, so a resting stick sends nothing.
const AXIS_EPSILON = 0.01;

export function attachGamepads({ send, onConnect = () => {}, log = () => {} }) {
  const last = new Map(); // pad slot -> { buttons: [], axes: [] }
  let enabled = false;
  let frame;

  const slot = (gamepad) => gamepad.index % MAX_PADS;
  const connected = (gamepad) => {
    const n = slot(gamepad);
    last.set(n, { buttons: [], axes: [] });
    send(Input.PAD_CONNECTED, { button: n, a: 1 });
    log('info', 'gamepad connected', { id: gamepad.id, mapping: gamepad.mapping, slot: n });
    onConnect(gamepad);
  };

  function poll() {
    frame = undefined;
    if (!enabled || !last.size) return;
    if (document.hasFocus()) {
      for (const g of navigator.getGamepads?.() ?? []) {
        if (!g) continue;
        const n = slot(g);
        const prev = last.get(n);
        if (!prev) continue;
        g.buttons.forEach((b, i) => {
          const v = b.value || (b.pressed ? 1 : 0);
          if (prev.buttons[i] !== v) {
            prev.buttons[i] = v;
            send(Input.PAD_BUTTON, { button: n, keycode: i, a: v });
          }
        });
        g.axes.forEach((v, i) => {
          if (prev.axes[i] === undefined || Math.abs(prev.axes[i] - v) > AXIS_EPSILON || (v === 0 && prev.axes[i] !== 0)) {
            prev.axes[i] = v;
            send(Input.PAD_AXIS, { button: n, keycode: i, a: v });
          }
        });
      }
    }
    frame = requestAnimationFrame(poll);
  }
  const start = () => { if (enabled && last.size && !frame) frame = requestAnimationFrame(poll); };

  addEventListener('gamepadconnected', (e) => {
    if (!enabled) return;
    connected(e.gamepad);
    start();
  });
  addEventListener('gamepaddisconnected', (e) => {
    const n = slot(e.gamepad);
    if (!last.delete(n) || !enabled) return;
    send(Input.PAD_CONNECTED, { button: n, a: 0 });
    log('info', 'gamepad disconnected', { slot: n });
  });

  return {
    /** On when the host supports controllers and this session may send input. */
    setEnabled(on) {
      if (on === enabled) return;
      enabled = on;
      if (!on) {
        for (const n of last.keys()) send(Input.PAD_CONNECTED, { button: n, a: 0 });
        last.clear();
        return;
      }
      // Controllers already plugged in (Chrome only lists them after a button press).
      for (const g of navigator.getGamepads?.() ?? []) if (g) connected(g);
      start();
    },
    /** After a reconnect: the host forgot its virtual pads, so announce them again. */
    resend() {
      if (!enabled) return;
      for (const [n, prev] of last) {
        prev.buttons = [];
        prev.axes = [];
        send(Input.PAD_CONNECTED, { button: n, a: 1 });
      }
    },
  };
}
