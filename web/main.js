// Main thread: wires input, UI and the worker together. Video and networking live
// in worker.js so the main thread only handles input and small DOM updates.

import { inputEvent } from './proto.js';
import { attachInput } from './input.js';
import { createUI } from './ui.js';
import { log } from './log.js';
import { MAC_KEYCODES as K } from './keymap.js';

// Shortcuts iPadOS keeps for itself (no web page or app can capture them), sent as one tap.
const CMD = K.MetaLeft;
const SHORTCUTS = [
  { label: 'App switcher', keysLabel: '⌘⇥', keys: [CMD, K.Tab] },
  { label: 'Spotlight', keysLabel: '⌘Space', keys: [CMD, K.Space] },
  { label: 'Mission Control', keysLabel: '⌃↑', keys: [K.ControlLeft, K.ArrowUp] },
  { label: 'Hide app', keysLabel: '⌘H', keys: [CMD, K.KeyH] },
  { label: 'Quit app', keysLabel: '⌘Q', keys: [CMD, K.KeyQ] },
  { label: 'Close window', keysLabel: '⌘W', keys: [CMD, K.KeyW] },
  { label: 'Screenshot area', keysLabel: '⌘⇧4', keys: [CMD, K.ShiftLeft, K.Digit4] },
  { label: 'Force quit…', keysLabel: '⌘⌥⎋', keys: [CMD, K.AltLeft, K.Escape] },
  { label: 'Lock screen', keysLabel: '⌃⌘Q', keys: [K.ControlLeft, CMD, K.KeyQ] },
];

const TOKEN_KEY = 'glass.token';
const SWAP_KEY = 'glass.swapCmdOption';
const CTRL_AS_CMD_KEY = 'glass.ctrlAsCmd';
const DECODER_STEP_KEY = 'glass.decoderStep';
const PC_KEYBOARD_KEY = 'glass.pcKeyboard';
const MACHINE_KEY = 'glass.machine';
// Give up on a remembered machine after this many failed connects and return to the
// machine that served this page.
const MACHINE_MAX_FAILURES = 3;
// ...or after this long without connecting: an unreachable host may never refuse, so
// attempts just time out slowly.
const MACHINE_CONNECT_TIMEOUT_MS = 12_000;
// Default to PC keyboard conventions on Linux, Windows and ChromeOS clients.
const isPC = /Linux|Windows|CrOS/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent);
const pcKeyboard = (storage((s) => s.getItem(PC_KEYBOARD_KEY)) ?? (isPC ? '1' : '0')) === '1';
const ctrlAsCmd = storage((s) => s.getItem(CTRL_AS_CMD_KEY)) !== '0'; // on by default

const screens = document.getElementById('screens');
const hud = document.getElementById('hud');
const keySink = document.getElementById('keys');

function storage(fn) {
  try { return fn(localStorage); } catch { return undefined; }
}

// The host prints a link with `#t=<token>`; remember it and drop it from the URL.
const hashToken = new URLSearchParams(location.hash.slice(1)).get('t');
if (hashToken) {
  storage((s) => s.setItem(TOKEN_KEY, hashToken));
  history.replaceState(null, '', location.pathname + location.search);
}
const token = hashToken ?? storage((s) => s.getItem(TOKEN_KEY));

const config = await fetch('config.json').then((r) => r.json());

// Reload into new client code when the host's web files change (checked on reconnect
// and when the tab comes back), so a long-lived tab never runs stale code.
async function reloadIfUpdated(reason) {
  try {
    const latest = await fetch('config.json').then((r) => r.json());
    if (latest.version && latest.version !== config.version) {
      log('info', `client updated (${reason}), reloading`, { from: config.version, to: latest.version });
      setTimeout(() => location.reload(), 300); // let the log flush
    }
  } catch {}
}
const worker = new Worker('worker.js', { type: 'module' });
worker.onerror = (e) => log('error', `worker: ${e.message}`, { src: `${e.filename}:${e.lineno}` });

const control = (msg) => worker.postMessage({ type: 'control', msg });
const send = (kind, fields) => worker.postMessage({ type: 'input', bytes: inputEvent(kind, fields) });

// Resolves with the next clipboard text the Mac reports (used for Cmd+C / Cmd+X).
let clipboardWaiters = [];
let lastMacClipboard;
function nextClipboard(timeoutMs) {
  return new Promise((resolve, reject) => {
    const waiter = { resolve, reject };
    clipboardWaiters.push(waiter);
    setTimeout(() => {
      if (clipboardWaiters.includes(waiter)) {
        clipboardWaiters = clipboardWaiters.filter((w) => w !== waiter);
        // Nothing new (e.g. copied the same text again): fall back to the last known value.
        if (lastMacClipboard !== undefined) resolve(lastMacClipboard);
        else reject(new Error('no clipboard update'));
      }
    }, timeoutMs);
  });
}

let statsVisible = true;
const input = attachInput({
  screens,
  keySink,
  send,
  control,
  nextClipboard,
  onLocalShortcut: (code) => (code === 'KeyF' ? toggleFullscreen() : toggleStats()),
  swapCmdOption: storage((s) => s.getItem(SWAP_KEY)) === '1',
  ctrlAsCmd,
  pcKeyboard,
  onZoom: (s) => {
    document.getElementById('zoom-pill').classList.toggle('hidden', s === 1);
    document.getElementById('zoom-level').textContent = `${s.toFixed(1)}×`;
  },
});
document.getElementById('zoom-reset').addEventListener('click', () => input.resetZoom());

const ui = createUI({
  onOpenMachines: loadMachines,
  onSelectMachine: (m) => switchMachine(machineId(m), m.name),
  onSelectDisplay: (id) => control(id === 'all' ? { t: 'showAll' } : { t: 'selectDisplay', id }),
  shortcuts: SHORTCUTS,
  swapCmdOption: storage((s) => s.getItem(SWAP_KEY)) === '1',
  ctrlAsCmd,
  pcKeyboard,
  onPcKeyboard: (on) => {
    storage((s) => s.setItem(PC_KEYBOARD_KEY, on ? '1' : '0'));
    input.setPcKeyboard(on);
    log('info', `pc keyboard ${on ? 'on' : 'off'}`);
  },
  onCtrlAsCmd: (on) => {
    storage((s) => s.setItem(CTRL_AS_CMD_KEY, on ? '1' : '0'));
    input.setCtrlAsCmd(on);
    log('info', `ctrl as cmd ${on ? 'on' : 'off'}`);
  },
  onSwapCmdOption: (on) => {
    storage((s) => s.setItem(SWAP_KEY, on ? '1' : '0'));
    input.setSwapCmdOption(on);
    log('info', `swap cmd/option ${on ? 'on' : 'off'}`);
  },
  onShortcut: (sc) => input.sendCombo(sc.keys),
  onKeyboard: () => input.focusKeys(),
  onUploadFiles: uploadFiles,
  onRequestFiles: () => {
    control({ t: 'pickFiles' });
    ui.toast({ text: 'Choose files in the dialog on your Mac' });
  },
  onAudio: toggleAudio,
  onFullscreen: toggleFullscreen,
  onToggleStats: toggleStats,
  onToken: (t) => {
    storage((s) => s.setItem(TOKEN_KEY, t));
    worker.postMessage({ type: 'token', token: t });
  },
});

// Launched from the home screen there's no browser UI to leave, so fullscreen is moot.
const standalone = matchMedia('(display-mode: standalone), (display-mode: fullscreen)').matches || navigator.standalone;
if (standalone) document.getElementById('btn-fullscreen').classList.add('hidden');
log('info', 'display mode', { standalone: !!standalone });

// No token is fine on your own tailnet; the host asks for one only if it can't identify us.

// The page can drive any Glass host on the tailnet: same client, different WebTransport
// endpoint. Start on the machine picked last time, if any.
const homeHost = config.wtHost ?? location.hostname;
let currentHost = storage((s) => s.getItem(MACHINE_KEY)) ?? homeHost;
let machineFailures = 0;
let machineTimer;

function giveUpOnMachine() {
  ui.toast({ text: `Couldn't reach ${currentHost}; back to this machine`, kind: 'error' });
  switchMachine(homeHost);
}

/** Falls back to the home machine if `currentHost` doesn't connect in time. */
function armMachineTimeout() {
  clearTimeout(machineTimer);
  if (currentHost !== homeHost) machineTimer = setTimeout(giveUpOnMachine, MACHINE_CONNECT_TIMEOUT_MS);
}
// A machine is "host" (default port) or "host:port".
const wtUrl = (machine) => `https://${machine.includes(':') ? machine : `${machine}:${config.wtPort}`}/`;
const machineId = (m) => (m.wt_port && m.wt_port !== config.wtPort ? `${m.dns}:${m.wt_port}` : m.dns);

worker.postMessage({
  type: 'init',
  url: wtUrl(currentHost),
  certHash: config.certHash,
  token,
  decoderStep: Number(storage((s) => s.getItem(DECODER_STEP_KEY)) ?? 0),
});
armMachineTimeout();

// Canvases are created on demand (one per host stream) and handed to the worker once;
// extras are hidden rather than destroyed so they can be reused.
const canvases = [];
function showStreams(count) {
  while (canvases.length < count) {
    const canvas = document.createElement('canvas');
    canvas.dataset.stream = String(canvases.length);
    canvas.tabIndex = -1;
    screens.append(canvas);
    input.observeCanvas(canvas);
    const offscreen = canvas.transferControlToOffscreen();
    worker.postMessage({ type: 'canvas', stream: canvases.length, canvas: offscreen }, [offscreen]);
    canvases.push(canvas);
  }
  canvases.forEach((c, i) => c.classList.toggle('hidden', i >= count));
  worker.postMessage({ type: 'streams', count });
}
showStreams(1);
if (document.visibilityState !== 'visible') worker.postMessage({ type: 'visible', visible: false });

const videoSizes = [];
let status = 'connecting…';
let codec = '';
let statsCount = 0;
let targetMbps;
let displays = [];
const transfers = new Map(); // id -> toast

worker.onmessage = ({ data }) => {
  switch (data.type) {
    case 'connected':
      reloadIfUpdated('reconnect');
      break;
    case 'status':
      if (data.level === 'error' && /connection failed/.test(data.text) && currentHost !== homeHost) {
        if (++machineFailures >= MACHINE_MAX_FAILURES) giveUpOnMachine();
      }
      status = data.text;
      hud.textContent = status;
      log(data.level ?? 'info', `status: ${status}`);
      break;
    case 'log':
      log(data.level, data.msg, data.data);
      break;
    case 'codec':
      codec = data.codec;
      log('info', `decoder configured: ${codec}`);
      break;
    case 'size':
      videoSizes[data.stream] = `${data.width}×${data.height}`;
      input.setVideoSize(data.stream, data.width, data.height);
      break;
    case 'stats':
      if (++statsCount % 5 === 0) log('info', 'stats', data);
      if (statsVisible) {
        hud.textContent =
          `${currentHost.split('.')[0]}  ${status}  ${codec}  ${videoSizes.filter(Boolean).join(' + ')}\n` +
          `latency ${data.latencyAvg.toFixed(1)} ms (max ${data.latencyMax.toFixed(1)})  rtt ${data.rtt.toFixed(1)} ms\n` +
          `${data.fps} fps  ${data.mbps.toFixed(1)} Mbps` +
          (targetMbps ? ` (target ${targetMbps.toFixed(0)})` : '') +
          `  dropped ${data.dropped}` +
          (data.audioPackets ? `\naudio ${data.audioPackets} pkt/s  lost ${data.audioLost}` : '');
      }
      break;
    case 'decoderStep':
      storage((s) => s.setItem(DECODER_STEP_KEY, String(data.step)));
      break;
    case 'unauthorized':
      storage((s) => s.removeItem(TOKEN_KEY));
      log('warn', 'token rejected');
      ui.showAuth();
      break;
    case 'control':
      onControl(data.msg);
      break;
    case 'fileProgress':
      onFileProgress(data);
      break;
    case 'fileReceived':
      onFileReceived(data);
      break;
    case 'audio':
      ui.setAudioOn(data.on);
      if (!data.on) audioContext?.suspend();
      log('info', `audio ${data.on ? 'on' : 'off'}`);
      break;
  }
};

function onControl(msg) {
  switch (msg.t) {
    case 'welcome':
      machineFailures = 0;
      clearTimeout(machineTimer);
      displays = msg.displays;
      // Nothing being streamed (e.g. capture failed): fall back to the first display.
      if (!msg.streams.length && displays.length) control({ t: 'selectDisplay', id: displays[0].id });
      ui.setAudioAvailable(msg.audio);
      input.setHostOs(msg.host_os ?? 'macos');
      ui.setHostOs(msg.host_os ?? 'macos');
      onLayout(msg.streams);
      log('info', 'welcome', { displays: displays.length, streams: msg.streams.map((d) => d.id), audio: msg.audio });
      break;
    case 'layout':
      onLayout(msg.streams);
      break;
    case 'bitrate':
      targetMbps = msg.bps / 1e6;
      break;
    case 'clipboard':
      log('info', 'mac clipboard changed', { length: msg.text.length });
      lastMacClipboard = msg.text;
      for (const w of clipboardWaiters) w.resolve(msg.text);
      clipboardWaiters = [];
      break;
    case 'fileSaved':
      ui.toast({ text: `Saved to Mac: ${msg.path.replace(/^\/Users\/[^/]+/, '~')}` });
      break;
    case 'error':
      if (msg.msg !== 'unauthorized') ui.toast({ text: msg.msg, kind: 'error', timeout: 6000 });
      log('error', `host: ${msg.msg}`);
      break;
  }
}

async function loadMachines() {
  ui.renderMachines(undefined, currentHost);
  try {
    const { machines } = await fetch('machines.json').then((r) => r.json());
    ui.renderMachines(machines.map((m) => ({ ...m, id: machineId(m) })), currentHost);
  } catch {
    ui.renderMachines([], currentHost);
  }
}

function switchMachine(host, name = host) {
  currentHost = host;
  machineFailures = 0;
  storage((s) => (host === homeHost ? s.removeItem(MACHINE_KEY) : s.setItem(MACHINE_KEY, host)));
  displays = [];
  armMachineTimeout();
  worker.postMessage({ type: 'switchHost', url: wtUrl(host) });
  log('info', 'switch machine', { host });
  ui.toast({ text: `Connecting to ${name}…`, timeout: 2500 });
}

function onLayout(streams) {
  for (const d of streams) if (!displays.some((x) => x.id === d.id)) displays.push(d);
  // The host orders streams as arranged in macOS; lay them out the same way.
  screens.dataset.arrange = arrangement(streams);
  videoSizes.length = streams.length;
  showStreams(streams.length);
  ui.renderDisplays(displays, streams.map((d) => d.id));
}

/** 'row' if the displays sit side by side in macOS, 'column' if stacked. */
function arrangement(list) {
  if (list.length < 2) return 'row';
  const spread = (key) => Math.max(...list.map((d) => d[key])) - Math.min(...list.map((d) => d[key]));
  return spread('x') >= spread('y') ? 'row' : 'column';
}

function uploadFiles(files) {
  for (const file of files) worker.postMessage({ type: 'sendFile', file });
}

function onFileProgress({ id, name, done, total, direction }) {
  let t = transfers.get(id);
  const verb = direction === 'up' ? 'Sending' : 'Receiving';
  const fraction = total ? done / total : 1;
  if (!t) {
    t = ui.toast({ text: `${verb} ${name}`, progress: fraction, timeout: 0 });
    transfers.set(id, t);
  }
  t.update({ progress: fraction });
  if (done >= total && direction === 'up') {
    t.update({ text: `Sent ${name}` });
    t.dismiss(1500);
    transfers.delete(id);
  }
}

function onFileReceived({ id, file }) {
  transfers.get(id)?.dismiss();
  transfers.delete(id);
  // Saving needs a user gesture on iPad, so offer a button.
  ui.toast({
    text: `${file.name} (${formatBytes(file.size)})`,
    timeout: 0,
    action: { label: 'Save', run: () => saveFile(file) },
  });
}

async function saveFile(file) {
  if (navigator.canShare?.({ files: [file] })) {
    try {
      await navigator.share({ files: [file] });
      return;
    } catch (e) {
      if (e.name === 'AbortError') return;
    }
  }
  const url = URL.createObjectURL(file);
  const a = document.createElement('a');
  a.href = url;
  a.download = file.name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

function formatBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(0)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}

let audioContext;
function toggleAudio() {
  if (!audioContext) {
    // Must be created and resumed inside the tap: Safari can start it suspended.
    // 'playback' keeps it audible when the iPad's silent mode is on.
    if (navigator.audioSession) navigator.audioSession.type = 'playback';
    audioContext = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
    audioContext.resume();
    audioContext.audioWorklet
      .addModule('audio-worklet.js')
      .then(() => {
        const node = new AudioWorkletNode(audioContext, 'glass-audio', { outputChannelCount: [2] });
        node.connect(audioContext.destination);
        worker.postMessage({ type: 'audioPort', port: node.port }, [node.port]);
        worker.postMessage({ type: 'audioToggle' });
        log('info', 'audio started', { sampleRate: audioContext.sampleRate, baseLatency: audioContext.baseLatency });
      })
      .catch((e) => log('error', `audio init failed: ${e.message ?? e}`));
    return;
  }
  if (audioContext.state === 'suspended') audioContext.resume();
  worker.postMessage({ type: 'audioToggle' });
}

function toggleStats() {
  statsVisible = !statsVisible;
  hud.classList.toggle('hidden', !statsVisible);
}

async function toggleFullscreen() {
  if (document.fullscreenElement) {
    navigator.keyboard?.unlock();
    await document.exitFullscreen();
    return;
  }
  await document.documentElement.requestFullscreen?.();
  // Chromium only: lets Cmd+W and similar shortcuts reach the remote Mac.
  await navigator.keyboard?.lock?.().catch(() => {});
}

// iPadOS suspends background tabs; reconnect as soon as we're back.
document.addEventListener('visibilitychange', () => {
  log('info', `visibility: ${document.visibilityState}`);
  worker.postMessage({ type: 'visible', visible: document.visibilityState === 'visible' });
  if (document.visibilityState === 'visible') {
    worker.postMessage({ type: 'wake' });
    reloadIfUpdated('visible');
  }
});
addEventListener('online', () => worker.postMessage({ type: 'wake' }));
