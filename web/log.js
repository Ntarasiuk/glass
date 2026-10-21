// Forwards client logs to the host (logs/client.log) so problems on remote
// devices are visible without a debugger attached. Batched once per second.

const queue = [];
const session = Math.random().toString(36).slice(2, 8);

export function log(level, msg, data) {
  queue.push(JSON.stringify({ t: Date.now(), session, level, msg, ...(data && { data }) }));
  if (level === 'error') console.error(msg, data ?? '');
}

// Remote access (glassdesk.dev/app/) has no host web server to post logs to: they go
// over the connection instead, once it's signed in (main.js sets the sink).
const remote = new URLSearchParams(location.hash.slice(1)).has('r');
const REMOTE_QUEUE_MAX = 500;
let sink;

/** `send(lines)` delivers logs over the connection; undefined while it's not usable. */
export function setLogSink(send) {
  sink = send;
  if (sink) flush();
}

function flush() {
  if (!queue.length) return;
  if (remote) {
    if (sink) sink(queue.splice(0, 200));
    else if (queue.length > REMOTE_QUEUE_MAX) queue.splice(0, queue.length - REMOTE_QUEUE_MAX);
    return;
  }
  const body = queue.splice(0).join('\n');
  fetch('log', { method: 'POST', body, keepalive: true }).catch(() => {});
}

setInterval(flush, 1000);
addEventListener('pagehide', flush);

addEventListener('error', (e) => log('error', `uncaught: ${e.message}`, { src: `${e.filename}:${e.lineno}:${e.colno}` }));
addEventListener('unhandledrejection', (e) => log('error', `unhandled rejection: ${e.reason?.message ?? e.reason}`, { stack: e.reason?.stack }));

log('info', 'client start', {
  ua: navigator.userAgent,
  url: location.href,
  screen: `${screen.width}x${screen.height}@${devicePixelRatio}`,
  features: {
    webTransport: typeof WebTransport !== 'undefined',
    videoDecoder: typeof VideoDecoder !== 'undefined',
    offscreenCanvas: typeof OffscreenCanvas !== 'undefined',
    pointerRawUpdate: 'onpointerrawupdate' in window,
    keyboardLock: !!navigator.keyboard?.lock,
  },
});
