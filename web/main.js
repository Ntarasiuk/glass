// Main thread: wires input, UI and the worker together. Video and networking live
// in worker.js so the main thread only handles input and small DOM updates.

import { inputEvent } from './proto.js';
import { attachInput } from './input.js';
import { createUI } from './ui.js';
import { log, setLogSink } from './log.js';
import { MAC_KEYCODES as K } from './keymap.js';
import { hostIdFor, serveRtc } from './rtc.js';
import { createChat } from './chat.js';

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
const RESOLUTION_KEY = 'glass.resolution'; // 'auto' (sized to this screen) or 'native'
const PASS_KEY = 'glass.pass'; // guest pass from a magic link: { pass, expires }
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
// Phones and iPads freeze hidden pages and have few hardware decoders, so a hidden tab
// gives its decoders up at once (iPadOS reports itself as a Mac with a touchscreen).
const mobile = /iPhone|iPad|Android/.test(navigator.userAgent) || (/Macintosh/.test(navigator.userAgent) && navigator.maxTouchPoints > 1);
// Desktop browsers also call a window hidden whenever another one covers it (every Alt+Tab
// to a maximized app on Windows). Keep streaming that long, so switching back is instant
// instead of waiting for a keyframe and a new decoder.
const PAUSE_HIDDEN_AFTER_MS = 60_000;
const ctrlAsCmd = storage((s) => s.getItem(CTRL_AS_CMD_KEY)) !== '0'; // on by default

const screens = document.getElementById('screens');
const hud = document.getElementById('hud');
const keySink = document.getElementById('keys');

function storage(fn) {
  try { return fn(localStorage); } catch { return undefined; }
}

// The host prints a link with `#t=<token>`; remember it and drop it from the URL.
// A magic link carries `#l=<code>` instead: single use, so it's never stored.
// Remote access (glassdesk.dev/app/#r=<host key>) keeps `r` in the URL: it names the
// computer, and isn't secret.
const hashParams = new URLSearchParams(location.hash.slice(1));
const hashToken = hashParams.get('t');
const link = hashParams.get('l') ?? undefined;
const remoteKey = hashParams.get('r') ?? undefined;
const remoteId = remoteKey && (await hostIdFor(remoteKey));
// Every remote computer shares this page's origin, so credentials are kept per computer:
// one computer's token must never be offered to another.
const tokenKey = remoteId ? `${TOKEN_KEY}.${remoteId}` : TOKEN_KEY;
const passKey = remoteId ? `${PASS_KEY}.${remoteId}` : PASS_KEY;
if (hashToken) storage((s) => s.setItem(tokenKey, hashToken));
if (hashToken || link) history.replaceState(null, '', location.pathname + location.search + (remoteKey ? `#r=${remoteKey}` : ''));
const token = hashToken ?? storage((s) => s.getItem(tokenKey));
const savedPass = (() => {
  try {
    const p = JSON.parse(storage((s) => s.getItem(passKey)) ?? 'null');
    return p && p.expires * 1000 > Date.now() ? p.pass : undefined;
  } catch { return undefined; }
})();
let guest = false; // signed in with a magic link: no switching machines, no updating the host
// 'auto', 'native', or 'fit:WxH' ("fit this device": lasts only for this tab, so a later
// visit never reshapes the Mac's displays by itself).
let resolutionMode = (() => {
  try { const fit = sessionStorage.getItem('glass.fit'); if (fit) return fit; } catch {}
  return storage((s) => s.getItem(RESOLUTION_KEY)) === 'native' ? 'native' : 'auto';
})();
// Remote video over WebRTC (see videoFor / showStreams): declared early, set up below.
let rtc;
let rtpVideo = false;
let streamCount = 1;
const videos = [];
// Dynamic resolution: tell the host how many device pixels each display gets here (its
// area × pixel ratio × zoom), so it encodes no more and no less. "This Mac" asks for full
// resolution. Sizes come from ResizeObserver, so measuring never forces a layout.
let zoomScale = 1;
const areas = new Map(); // element -> [css width, css height]
const areaObserver = new ResizeObserver((entries) => {
  for (const e of entries) areas.set(e.target, [e.contentRect.width, e.contentRect.height]);
  queueViewport();
});
matchMedia(`(resolution: ${devicePixelRatio}dppx)`).addEventListener?.('change', () => queueViewport());
let viewportTimer;
let lastViewport = '';

// Remote: no host serves this page, so there's no config to fetch.
const config = remoteKey
  ? { wtPort: 0, wtHost: null, certHash: null, version: null }
  : await fetch('config.json').then((r) => r.json());

// Reload into new client code when the host's web files change (checked on reconnect
// and when the tab comes back), so a long-lived tab never runs stale code.
async function reloadIfUpdated(reason) {
  if (remoteKey) return;
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
// A view-only link sends no input (the host would drop it anyway).
let inputAllowed = true;
const send = (kind, fields) => inputAllowed && worker.postMessage({ type: 'input', bytes: inputEvent(kind, fields) });
let session = 0; // this session's id, from welcome (chat from it is our own)
let perms = { input: true, clipboard: true, files: true, owner: true };
let features = {};

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

const statusEl = document.getElementById('status');
// Friendly machine names from the last machines.json, by machine id.
let machineNames = {};
const STATS_KEY = 'glass.stats';
let statsVisible = storage((s) => s.getItem(STATS_KEY)) === '1';
hud.classList.toggle('hidden', !statsVisible);
let tokenTried = Boolean(token);
let restarting; // 'Glass' or 'computer' after a restart request, until it's back
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
    zoomScale = s;
    queueViewport();
    document.getElementById('zoom-pill').classList.toggle('hidden', s === 1);
    document.getElementById('zoom-level').textContent = `${s.toFixed(1)}×`;
  },
});
document.getElementById('zoom-reset').addEventListener('click', () => input.resetZoom());

const ui = createUI({
  onOpenMachines: loadMachines,
  onWakeMachine: (m) => control({ t: 'wake', dns: m.dns }),
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
  resolution: resolutionMode,
  onResolution: (mode) => setResolution(mode),
  fitSizes: () => fitSizes(),
  deviceName: () => deviceLabel().replace(/^.* on /, ''),
  onKeyboard: () => input.focusKeys(),
  onUploadFiles: uploadFiles,
  onRequestFiles: () => {
    control({ t: 'pickFiles' });
    ui.toast({ text: 'Choose files in the dialog on your Mac' });
  },
  onAudio: toggleAudio,
  onFullscreen: toggleFullscreen,
  onToggleStats: toggleStats,
  onPrivacy: (on) => control({ t: 'privacy', on }),
  onRestart: (glass) => {
    restarting = glass ? 'Glass' : 'computer';
    control({ t: 'restart', glass });
    log('info', `restart ${restarting}`);
    onConnectionStatus('restarting');
  },
  onToken: (t) => {
    tokenTried = true;
    showStatus('connecting', 'Connecting', machineName(currentHost));
    storage((s) => s.setItem(tokenKey, t));
    worker.postMessage({ type: 'token', token: t });
  },
});

const chat = createChat({ onSend: (text) => control({ t: 'chat', text }), toast: (o) => ui.toast(o) });
chat.setAvailable(false);

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

// Remote access: the WebRTC connection lives here (the main thread) and is relayed to the
// worker over a MessagePort.
let rtcPort;
if (remoteKey) {
  const channel = new MessageChannel();
  const signalUrl = `${location.origin.replace(/^http/, 'ws')}/signal/connect/${remoteId}`;
  rtc = serveRtc({
    port: channel.port1,
    hostKey: remoteKey,
    signalUrl,
    log,
    onVideo: (stream, track) => {
      const v = videoFor(stream);
      v.srcObject = new MediaStream([track]);
      v.play().catch(() => {});
    },
    onMode: (rtp) => {
      rtpVideo = rtp;
      codec = rtp ? 'H.264 · WebRTC' : codec;
      log('info', `remote video: ${rtp ? 'RTP tracks' : 'data channel'}`);
      showStreams(streamCount);
    },
  });
  rtcPort = channel.port2;
}

worker.postMessage({
  type: 'init',
  url: wtUrl(currentHost),
  certHash: config.certHash,
  token,
  link,
  pass: savedPass,
  rtcPort,
  device: deviceLabel(),
  decoderStep: Number(storage((s) => s.getItem(DECODER_STEP_KEY)) ?? 0),
}, rtcPort ? [rtcPort] : []);
armMachineTimeout();
showStatus('connecting', 'Connecting', machineName(currentHost));
ui.setStatsPressed(statsVisible);

// Canvases are created on demand (one per host stream) and handed to the worker once;
// extras are hidden rather than destroyed so they can be reused.
const canvases = [];
// Remote viewers get video as WebRTC tracks, shown in <video> elements in place of the
// worker's canvases (input maps onto either: both carry data-stream).

function videoFor(stream) {
  while (videos.length <= stream) {
    const i = videos.length;
    const v = document.createElement('video');
    v.dataset.stream = String(i);
    v.tabIndex = -1;
    v.autoplay = true;
    v.muted = true;
    v.playsInline = true;
    v.disablePictureInPicture = true;
    v.disableRemotePlayback = true;
    v.setAttribute('playsinline', '');
    v.style.order = String(i);
    v.classList.add('hidden');
    v.addEventListener('resize', () => {
      if (!v.videoWidth) return;
      videoSizes[i] = `${v.videoWidth}×${v.videoHeight}`;
      input.setVideoSize(i, v.videoWidth, v.videoHeight);
      hideStatus();
    });
    screens.append(v);
    input.observeCanvas(v);
    areaObserver.observe(v);
    videos.push(v);
  }
  return videos[stream];
}

function showStreams(count) {
  streamCount = count;
  if (rtpVideo) {
    for (let i = 0; i < count; i++) videoFor(i);
    videos.forEach((v, i) => v.classList.toggle('hidden', i >= count));
    canvases.forEach((c) => c.classList.add('hidden'));
    return;
  }
  videos.forEach((v) => v.classList.add('hidden'));
  while (canvases.length < count) {
    const canvas = document.createElement('canvas');
    canvas.dataset.stream = String(canvases.length);
    canvas.style.order = String(canvases.length);
    canvas.tabIndex = -1;
    screens.append(canvas);
    input.observeCanvas(canvas);
    areaObserver.observe(canvas);
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
      if (rtpVideo) checkRtpVideoArrives();
      break;
    case 'status':
      if (data.level === 'error' && /connection failed/.test(data.text) && currentHost !== homeHost) {
        if (++machineFailures >= MACHINE_MAX_FAILURES) giveUpOnMachine();
      }
      status = data.text;
      // Logs wait (in log.js) until the next session is signed in.
      if (status !== 'connected') setLogSink(undefined);
      onConnectionStatus(status);
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
      hideStatus();
      break;
    case 'stats':
      // Over RTP the worker sees no video; the connection's own stats describe it.
      if (rtpVideo) data = { ...data, ...rtpStats };
      if (++statsCount % 5 === 0) log('info', 'stats', data);
      if (data.fps > 0) hideStatus();
      renderHud(data);
      break;
    case 'decoderStep':
      storage((s) => s.setItem(DECODER_STEP_KEY, String(data.step)));
      break;
    case 'unauthorized':
      storage((s) => s.removeItem(tokenKey));
      storage((s) => s.removeItem(passKey));
      log('warn', 'token rejected');
      hideStatus();
      ui.showAuth({ rejected: tokenTried });
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
      if (remoteKey) setLogSink((lines) => control({ t: 'log', lines }));
      guest = Boolean(msg.guest);
      // Remote: other machines aren't reachable from here, so no machines menu either.
      ui.setGuest(guest || Boolean(remoteKey));
      ui.setCanFit(!guest);
      session = msg.session ?? 0;
      perms = msg.perms ?? perms;
      features = msg.features ?? {};
      inputAllowed = perms.input;
      ui.setPerms(perms);
      chat.setAvailable(true);
      restarting = undefined;
      ui.setComputerControls({ isOwner: msg.perms ? msg.perms.owner : !guest, privacy: Boolean(msg.features?.privacy) });
      ui.setPrivacyOn(false);
      if (guest && resolutionMode.startsWith('fit:')) setResolution('auto');
      onLayout(msg.streams);
      queueViewport(true);
      // A reload or reconnect: the host put the displays back when the session ended.
      if (resolutionMode.startsWith('fit:') && !guest) sendFit();
      log('info', 'welcome', { displays: displays.length, streams: msg.streams.map((d) => d.id), audio: msg.audio });
      break;
    case 'layout':
      onLayout(msg.streams);
      break;
    case 'waiting':
      showStatus('connecting', 'Waiting to be let in', msg.msg);
      break;
    case 'denied':
      showStatus('error', 'Not let in', msg.msg);
      ui.toast({ text: msg.msg, kind: 'error', timeout: 0, action: { label: 'Ask again', run: () => {
        showStatus('connecting', 'Connecting', machineName(currentHost));
        worker.postMessage({ type: 'askAgain' });
      } } });
      break;
    case 'notice':
      ui.toast({ text: msg.msg });
      break;
    case 'chat':
      if (msg.sender !== session) chat.receive(msg);
      break;
    case 'pass':
      storage((s) => s.setItem(passKey, JSON.stringify({ pass: msg.pass, expires: msg.expires })));
      ui.toast({ text: `You're in. This device has access until ${formatExpiry(msg.expires)}.`, icon: 'link', timeout: 6000 });
      log('info', 'signed in with a magic link', { expires: msg.expires });
      break;
    case 'privacy':
      ui.setPrivacyOn(msg.on);
      log('info', `privacy screen ${msg.on ? 'on' : 'off'}`);
      break;
    case 'bitrate':
      targetMbps = msg.bps / 1e6;
      break;
    case 'updateAvailable':
      offerUpdate(msg);
      break;
    case 'clipboard':
      log('info', 'mac clipboard changed', { length: msg.text.length });
      lastMacClipboard = msg.text;
      for (const w of clipboardWaiters) w.resolve(msg.text);
      clipboardWaiters = [];
      break;
    case 'fileSaved':
      ui.toast({ text: `Saved on ${machineName(currentHost)}: ${msg.path.replace(/^\/(Users|home)\/[^/]+/, '~')}`, icon: 'file' });
      break;
    case 'error':
      if (msg.msg !== 'unauthorized') ui.toast({ text: msg.msg, kind: 'error', timeout: 6000 });
      log('error', `host: ${msg.msg}`);
      break;
  }
}

// ---- Connection state overlay and stats HUD ----


/** "Safari on iPad", "Chrome on Windows": how the host lists this viewer. */
function deviceLabel() {
  const ua = navigator.userAgent;
  const browser = /Edg\//.test(ua) ? 'Edge' : /Firefox\//.test(ua) ? 'Firefox'
    : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : 'A browser';
  // iPadOS reports itself as a Mac; touch gives it away.
  const os = /iPhone/.test(ua) ? 'iPhone'
    : /iPad/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1) ? 'iPad'
    : /Android/.test(ua) ? 'Android' : /CrOS/.test(ua) ? 'ChromeOS' : /Windows/.test(ua) ? 'Windows'
    : /Macintosh/.test(ua) ? 'Mac' : /Linux/.test(ua) ? 'Linux' : '';
  return os ? `${browser} on ${os}` : browser;
}

function machineName(id) {
  if (remoteKey) return 'your computer';
  return machineNames[id] ?? id.split('.')[0].split(':')[0];
}

function showStatus(state, title, detail = '') {
  statusEl.dataset.state = state;
  document.getElementById('status-title').textContent = title;
  document.getElementById('status-detail').textContent = detail;
  statusEl.classList.remove('gone');
}

function hideStatus() {
  statusEl.classList.add('gone');
}

/** Maps the worker's status text to what the person should see. */
function onConnectionStatus(text) {
  const name = machineName(currentHost);
  // Restarting: the connection drops and retries for a while; that's expected.
  if (restarting && text !== 'connected' && !text.startsWith('waiting for access token')) {
    showStatus('reconnecting', 'Restarting…', restarting === 'Glass' ? `Glass is restarting on ${name}. Reconnecting…` : `${name} is restarting. This reconnects once it's back and someone is logged in.`);
    return;
  }
  if (text === 'connected') showStatus('connecting', `Connected to ${name}`, 'Waiting for the screen…');
  else if (text.startsWith('disconnected')) showStatus('reconnecting', 'Reconnecting', `Lost the connection to ${name}. Trying again…`);
  else if (text.startsWith('connection failed') && remoteKey) {
    // Remote: the reason is specific (offline, identity check, no route), so show it.
    const reason = text.replace(/^connection failed: /, '').replace(/, retrying…$/, '');
    showStatus('error', `Can't reach ${name}`, `${reason} Retrying…`);
  } else if (text.startsWith('connection failed'))
    showStatus('error', `Can't reach ${name}`, 'Retrying. Make sure Glass is running there and both devices are on Tailscale.');
  else if (text.startsWith('waiting for access token')) hideStatus();
  else if (text === 'not let in') showStatus('error', 'Not let in', `The person at ${name} didn't let you in.`);
}

function hudLine(...parts) {
  const div = document.createElement('div');
  div.append(...parts);
  return div;
}

function latencyClass(ms) {
  return ms < 40 ? 'good' : ms < 80 ? 'warn' : 'bad';
}

function renderHud(d) {
  const host = document.createElement('b');
  host.textContent = machineName(currentHost);
  const latency = document.createElement('span');
  latency.className = latencyClass(d.latencyAvg);
  latency.textContent = `${d.latencyAvg.toFixed(1)} ms`;
  const lines = [
    hudLine(host, `  ${status}  ${codec}  ${videoSizes.filter(Boolean).join(' + ')}`),
    hudLine('latency ', latency, ` (max ${d.latencyMax.toFixed(1)})  rtt ${d.rtt.toFixed(1)} ms`),
    hudLine(
      `${d.fps} fps  ${d.mbps.toFixed(1)} Mbps` + (targetMbps ? ` (target ${targetMbps.toFixed(0)})` : '') + `  dropped ${d.dropped}`,
    ),
  ];
  if (d.audioPackets) lines.push(hudLine(`audio ${d.audioPackets} pkt/s  lost ${d.audioLost}`));
  hud.replaceChildren(...lines);
}

async function loadMachines() {
  ui.renderMachines(undefined, currentHost);
  try {
    const { machines } = await fetch('machines.json').then((r) => r.json());
    machineNames = Object.fromEntries(machines.map((m) => [machineId(m), m.name]));
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
  showStatus('connecting', `Connecting to ${name}`);
  armMachineTimeout();
  worker.postMessage({ type: 'switchHost', url: wtUrl(host) });
  log('info', 'switch machine', { host });
  ui.toast({ text: `Connecting to ${name}…`, timeout: 2500 });
}

/** If tracks were negotiated but no picture comes, go back to data-channel video. */
let rtpCheck;
function checkRtpVideoArrives() {
  clearTimeout(rtpCheck);
  rtpCheck = setTimeout(() => {
    if (!rtpVideo || videos.some((v) => v.videoWidth > 0)) return;
    log('warn', 'remote video: no picture over RTP; falling back to the data channel');
    try { sessionStorage.setItem('glass.rtp', '0'); } catch {}
    location.reload();
  }, 6000);
}

// Video numbers over RTP, from the connection's stats (once a second, no layout reads).
const rtpStats = {};
let lastInbound;
setInterval(async () => {
  if (!rtpVideo || !rtc) return;
  const report = await rtc.stats?.();
  if (!report) return;
  let frames = 0, bytes = 0, dropped = 0, jitter = 0, emitted = 0, decode = 0, decoded = 0, rtt;
  for (const r of report.values()) {
    if (r.type === 'inbound-rtp' && r.kind === 'video') {
      frames += r.framesDecoded ?? 0;
      bytes += r.bytesReceived ?? 0;
      dropped += (r.framesDropped ?? 0) + (r.freezeCount ?? 0);
      jitter += r.jitterBufferDelay ?? 0;
      emitted += r.jitterBufferEmittedCount ?? 0;
      decode += r.totalDecodeTime ?? 0;
      decoded += r.framesDecoded ?? 0;
    } else if (r.type === 'candidate-pair' && r.nominated && r.currentRoundTripTime != null) {
      rtt = r.currentRoundTripTime * 1000;
    }
  }
  const now = { frames, bytes, dropped, jitter, emitted, decode, decoded };
  const was = lastInbound ?? now;
  lastInbound = now;
  const dEmitted = emitted - was.emitted;
  const dDecoded = decoded - was.decoded;
  // One-way network time + jitter buffer + decode: what the viewer waits beyond capture.
  const buffer = dEmitted > 0 ? ((jitter - was.jitter) / dEmitted) * 1000 : 0;
  const decodeMs = dDecoded > 0 ? ((decode - was.decode) / dDecoded) * 1000 : 0;
  const latency = (rtt ?? 0) / 2 + buffer + decodeMs;
  Object.assign(rtpStats, {
    fps: frames - was.frames,
    mbps: ((bytes - was.bytes) * 8) / 1e6,
    dropped: dropped - was.dropped,
    latencyAvg: latency,
    latencyMax: latency,
    ...(rtt != null && { rtt }),
    jitterBufferMs: buffer,
  });
}, 1000);

function queueViewport(force = false) {
  if (force) lastViewport = '';
  clearTimeout(viewportTimer);
  viewportTimer = setTimeout(sendViewport, 300);
}
function sendViewport() {
  const elements = rtpVideo ? videos : canvases;
  const streams = [];
  for (let i = 0; i < streamCount; i++) {
    const area = areas.get(elements[i]);
    if (resolutionMode !== 'auto' || !area || !area[0]) {
      streams.push(null);
      continue;
    }
    const scale = devicePixelRatio * zoomScale;
    streams.push([Math.round(area[0] * scale), Math.round(area[1] * scale)]);
  }
  const msg = JSON.stringify(streams);
  if (msg === lastViewport) return;
  lastViewport = msg;
  control({ t: 'viewport', streams });
}

function setResolution(mode) {
  const wasFit = resolutionMode.startsWith('fit:');
  resolutionMode = mode;
  if (mode.startsWith('fit:')) {
    try { sessionStorage.setItem('glass.fit', mode); } catch {}
    sendFit();
  } else {
    try { sessionStorage.removeItem('glass.fit'); } catch {}
    storage((s) => s.setItem(RESOLUTION_KEY, mode));
    if (wasFit && !guest) control({ t: 'fit', size: null });
  }
  ui.setResolution(mode);
  queueViewport(true);
  log('info', `resolution: ${mode}`);
}

function sendFit() {
  const [w, h] = resolutionMode.slice(4).split('x').map(Number);
  if (w && h) control({ t: 'fit', size: [w, h] });
}

/**
 * Desktop sizes in this device's shape (its viewing area, in device pixels, at full, ¾ and
 * ½ size), when that shape differs from the Mac display's. Empty when they already match.
 */
function fitSizes() {
  const native = displays.find((d) => d.main) ?? displays[0];
  if (!native) return [];
  const dpr = devicePixelRatio;
  const w = Math.round(innerWidth * dpr);
  const h = Math.round(innerHeight * dpr);
  const mine = w / h;
  const theirs = native.width / native.height;
  if (Math.abs(mine - theirs) < theirs * 0.03) return [];
  const even = (n) => Math.round(n / 2) * 2;
  return [1, 0.75, 0.5]
    .map((f) => [even(w * f), even(h * f)])
    .filter(([a, b]) => Math.max(a, b) >= 1024 && Math.min(a, b) >= 640 && a <= 7680 && b <= 4320);
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

let updateOffered = false;
/** "6:40 PM" today, "Tue 6:40 PM" this week, else "Oct 28, 6:40 PM". */
function formatExpiry(unixSecs) {
  const d = new Date(unixSecs * 1000);
  const days = (d - Date.now()) / 86400e3;
  const opts = days < 1 && d.getDate() === new Date().getDate()
    ? { hour: 'numeric', minute: '2-digit' }
    : days < 6 ? { weekday: 'short', hour: 'numeric', minute: '2-digit' }
    : { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' };
  return d.toLocaleString(undefined, opts);
}

function offerUpdate({ current, version }) {
  if (updateOffered || guest) return;
  updateOffered = true;
  ui.toast({
    text: `Update available: Glass ${version} (you have ${current})`,
    icon: 'update',
    timeout: 0,
    action: {
      label: 'Update',
      run: () => {
        control({ t: 'update' });
        log('info', 'update requested', { from: current, to: version });
        ui.toast({ text: `Updating to ${version}… Glass reconnects when it's done.`, timeout: 15_000 });
      },
    },
  });
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
    icon: 'file',
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
  ui.setStatsPressed(statsVisible);
  storage((s) => s.setItem(STATS_KEY, statsVisible ? '1' : '0'));
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
let pauseTimer;
document.addEventListener('visibilitychange', () => {
  log('info', `visibility: ${document.visibilityState}`);
  clearTimeout(pauseTimer);
  if (document.visibilityState === 'visible') {
    worker.postMessage({ type: 'visible', visible: true });
    worker.postMessage({ type: 'wake' });
    reloadIfUpdated('visible');
    return;
  }
  const pause = () => worker.postMessage({ type: 'visible', visible: false });
  // Synchronously on mobile: the page may be frozen before a timer could run.
  if (mobile) pause();
  else pauseTimer = setTimeout(pause, PAUSE_HIDDEN_AFTER_MS);
});
addEventListener('online', () => worker.postMessage({ type: 'wake' }));
