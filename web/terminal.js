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
