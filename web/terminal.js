// A shell on the host (owner only), in a panel over the remote screen. xterm.js loads the
// first time it's opened. Bytes travel base64 over the control stream ({t: 'term', op}).

const $ = (id) => document.getElementById(id);

function toBase64(bytes) {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

function fromBase64(text) {
  const s = atob(text);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

const HEIGHT_KEY = 'glass.terminalHeight';
const FULL_KEY = 'glass.terminalFull';
const MIN_HEIGHT = 160;

function stored(key) {
  try { return localStorage.getItem(key); } catch { return null; }
}
function store(key, value) {
  try { localStorage.setItem(key, value); } catch {}
}

export function createTerminal({ control, log = () => {} }) {
  const panel = $('terminal');
  const mount = $('terminal-screen');
  const button = $('btn-terminal');
  const encoder = new TextEncoder();
  let term;
  let fit;
  let loading;
  let running = false;
  let available = false;

  async function load() {
    if (term) return;
    loading ??= (async () => {
      const css = document.createElement('link');
      css.rel = 'stylesheet';
      css.href = 'vendor/xterm.css';
      document.head.append(css);
      const [{ Terminal }, { FitAddon }] = await Promise.all([import('./vendor/xterm.mjs'), import('./vendor/addon-fit.mjs')]);
      term = new Terminal({
        cursorBlink: true,
        fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
        fontSize: 13,
        theme: { background: '#0b0c0e', foreground: '#f2f3f5', cursor: '#5cb2ff', selectionBackground: 'rgb(92 178 255 / 0.35)' },
      });
      fit = new FitAddon();
      term.loadAddon(fit);
      term.open(mount);
      term.onData((data) => {
        if (!running) {
          // After the shell exits, any key starts a new one.
          start();
          return;
        }
        control({ t: 'term', op: 'input', data: toBase64(encoder.encode(data)) });
      });
      term.onResize(({ cols, rows }) => running && control({ t: 'term', op: 'resize', cols, rows }));
      new ResizeObserver(() => !panel.classList.contains('hidden') && fit.fit()).observe(mount);
    })();
    await loading;
  }

  function start() {
    running = true;
    control({ t: 'term', op: 'open', cols: term.cols, rows: term.rows });
    log('info', 'terminal opened', { cols: term.cols, rows: term.rows });
  }

  async function open() {
    panel.classList.remove('hidden');
    button.setAttribute('aria-pressed', 'true');
    await load();
    fit.fit();
    if (!running) start();
    term.focus();
  }

  function close() {
    panel.classList.add('hidden');
    button.setAttribute('aria-pressed', 'false');
    term?.blur();
  }

  $('terminal-close').addEventListener('click', (e) => {
    e.preventDefault();
    close();
  });
  $('terminal-end').addEventListener('click', (e) => {
    e.preventDefault();
    if (running) control({ t: 'term', op: 'close' });
    running = false;
    term?.reset();
    close();
  });
  // Size: drag the top edge; full screen from the header button (or a double-click on it).
  const fullButton = $('terminal-full');
  const maxHeight = () => innerHeight - 20;
  function setHeight(px) {
    panel.style.setProperty('--term-height', `${Math.round(Math.max(MIN_HEIGHT, Math.min(px, maxHeight())))}px`);
  }
  function setFull(on) {
    panel.classList.toggle('full', on);
    fullButton.setAttribute('aria-pressed', String(on));
    fullButton.textContent = on ? 'Exit full screen' : 'Full screen';
    store(FULL_KEY, on ? '1' : '0');
  }
  const savedHeight = Number(stored(HEIGHT_KEY));
  if (savedHeight) setHeight(savedHeight);
  setFull(stored(FULL_KEY) === '1');
  fullButton.addEventListener('click', (e) => {
    e.preventDefault();
    setFull(!panel.classList.contains('full'));
    term?.focus();
  });
  panel.querySelector('header').addEventListener('dblclick', (e) => {
    if (e.target.closest('button')) return;
    setFull(!panel.classList.contains('full'));
  });
  const grip = $('terminal-grip');
  grip.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    grip.setPointerCapture(e.pointerId);
    panel.classList.add('resizing');
    // The panel is anchored at the bottom, so its height is the distance to the pointer.
    const bottom = panel.getBoundingClientRect().bottom;
    let frame = 0;
    let y = e.clientY;
    const move = (ev) => {
      y = ev.clientY;
      frame ||= requestAnimationFrame(() => {
        frame = 0;
        setHeight(bottom - y);
      });
    };
    const end = () => {
      cancelAnimationFrame(frame);
      setHeight(bottom - y);
      panel.classList.remove('resizing');
      store(HEIGHT_KEY, String(Math.round(panel.getBoundingClientRect().height)));
      grip.removeEventListener('pointermove', move);
    };
    grip.addEventListener('pointermove', move);
    grip.addEventListener('pointerup', end, { once: true });
    grip.addEventListener('pointercancel', end, { once: true });
  });
  // Keyboard resizing for the separator.
  grip.tabIndex = 0;
  grip.addEventListener('keydown', (e) => {
    const step = e.shiftKey ? 80 : 20;
    const h = panel.getBoundingClientRect().height;
    if (e.key === 'ArrowUp') setHeight(h + step);
    else if (e.key === 'ArrowDown') setHeight(h - step);
    else return;
    e.preventDefault();
    store(HEIGHT_KEY, String(Math.round(panel.getBoundingClientRect().height)));
  });

  // Escape belongs to the shell (vim and friends); the close button ends the panel.
  panel.addEventListener('keydown', (e) => e.stopPropagation());
  panel.addEventListener('keyup', (e) => e.stopPropagation());

  return {
    toggle() {
      if (!available) return;
      if (panel.classList.contains('hidden')) open();
      else close();
    },
    /** Shown to the owner when the host has a terminal. */
    setAvailable(on) {
      available = on;
      button.classList.toggle('hidden', !on);
      if (!on) close();
    },
    /** A reconnect means a new session on the host: its old shell is gone. */
    reset() {
      if (running) term?.write('\r\n\x1b[2m[disconnected]\x1b[0m\r\n');
      running = false;
    },
    onMessage(msg) {
      if (!term) return;
      if (msg.op === 'output') term.write(fromBase64(msg.data));
      else if (msg.op === 'exit') {
        running = false;
        term.write(`\r\n\x1b[2m[exited${msg.code != null ? ` with ${msg.code}` : ''}; press a key for a new shell]\x1b[0m\r\n`);
      }
    },
  };
}
