// Drawing on the screen: in draw mode a pen draws over the remote screen, and everyone
// connected (and, on a Mac, the person at it) sees the strokes, which fade after a few
// seconds. Points are fractions of a display stream's picture, so they land on the same
// spot at any size. Strokes travel in chunks while drawing: { id, stream, color, points }.
//
// The overlay sits above the screens: in draw mode it takes the pointer, so nothing
// reaches the remote; otherwise it lets every event through.

export const COLORS = ['#ff453a', '#ffd60a', '#32d74b', '#0a84ff', '#ffffff'];
const LINE_WIDTH = 4;
const FADE_AFTER_MS = 6000;
const FADE_MS = 2000;
// While drawing, new points go out at most this often.
const SEND_EVERY_MS = 40;

export function createAnnotator({ screens, send }) {
  const overlay = document.createElement('canvas');
  overlay.id = 'annotate';
  overlay.setAttribute('aria-hidden', 'true');
  // Right after the screens, so the toolbar and panels stay on top of it.
  screens.after(overlay);
  const ctx = overlay.getContext('2d');

  const palette = document.createElement('div');
  palette.id = 'draw-palette';
  palette.className = 'glass hidden';
  palette.setAttribute('role', 'toolbar');
  palette.setAttribute('aria-label', 'Pen');
  let color = COLORS[0];
  for (const c of COLORS) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'swatch';
    b.style.setProperty('--swatch', c);
    b.setAttribute('aria-label', `Pen colour ${c}`);
    b.setAttribute('aria-pressed', String(c === color));
    b.addEventListener('click', () => {
      color = c;
      for (const other of palette.querySelectorAll('.swatch')) other.setAttribute('aria-pressed', String(other === b));
    });
    palette.append(b);
  }
  const clearButton = document.createElement('button');
  clearButton.type = 'button';
  clearButton.textContent = 'Clear';
  clearButton.addEventListener('click', () => {
    clearFrom(session);
    send({ t: 'annotateClear' });
  });
  const doneButton = document.createElement('button');
  doneButton.type = 'button';
  doneButton.textContent = 'Done';
  doneButton.addEventListener('click', () => setEnabled(false));
  palette.append(clearButton, doneButton);
  document.body.append(palette);

  let enabled = false;
  let session = 0;
  let onChange = () => {};
  // `${sender}:${id}` -> { sender, stream, color, points, updated }
  const strokes = new Map();
  let frame = 0;
  let dpr = 1;
  // Content rects of the stream elements, read once per gesture or after a resize.
  let rects;
  const invalidate = () => {
    rects = undefined;
    sizeOverlay();
    schedule();
  };
  new ResizeObserver(invalidate).observe(screens);

  function sizeOverlay() {
    dpr = devicePixelRatio || 1;
    const w = Math.round(innerWidth * dpr);
    const h = Math.round(innerHeight * dpr);
    if (overlay.width !== w || overlay.height !== h) {
      overlay.width = w;
      overlay.height = h;
    }
  }

  /** stream -> content rect of its picture (letterboxed by object-fit: contain). */
  function streamRects() {
    if (rects) return rects;
    rects = new Map();
    for (const el of screens.querySelectorAll('[data-stream]')) {
      const r = el.getBoundingClientRect();
      const vw = el.videoWidth || el.width;
      const vh = el.videoHeight || el.height;
      if (!vw || !vh || !r.width || !r.height) continue;
      const scale = Math.min(r.width / vw, r.height / vh);
      const w = vw * scale;
      const h = vh * scale;
      rects.set(Number(el.dataset.stream), { left: r.left + (r.width - w) / 2, top: r.top + (r.height - h) / 2, width: w, height: h });
    }
    return rects;
  }

  function schedule() {
    if (!frame) frame = requestAnimationFrame(draw);
  }

  function draw(now) {
    frame = 0;
    ctx.clearRect(0, 0, overlay.width, overlay.height);
    const map = streamRects();
    now = performance.now();
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.lineWidth = LINE_WIDTH * dpr;
    for (const [key, s] of strokes) {
      const age = now - s.updated;
      if (age > FADE_AFTER_MS + FADE_MS) {
        strokes.delete(key);
        continue;
      }
      const r = map.get(s.stream);
      if (!r || !s.points.length) continue;
      ctx.globalAlpha = age < FADE_AFTER_MS ? 1 : 1 - (age - FADE_AFTER_MS) / FADE_MS;
      ctx.strokeStyle = s.color;
      ctx.beginPath();
      s.points.forEach(([x, y], i) => {
        const px = (r.left + x * r.width) * dpr;
        const py = (r.top + y * r.height) * dpr;
        if (i === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      });
      if (s.points.length === 1) ctx.lineTo((r.left + s.points[0][0] * r.width) * dpr + 0.1, (r.top + s.points[0][1] * r.height) * dpr);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
    // Keep animating while anything is still fading.
    if (strokes.size) schedule();
  }

  function add(sender, { id, stream, color: c, points }) {
    if (!Array.isArray(points) || typeof id !== 'string' && typeof id !== 'number') return;
    const key = `${sender}:${id}`;
    let s = strokes.get(key);
    if (!s) {
      s = { sender, stream: Number(stream) || 0, color: typeof c === 'string' ? c : COLORS[0], points: [] };
      strokes.set(key, s);
    }
    for (const p of points) {
      if (Array.isArray(p) && Number.isFinite(p[0]) && Number.isFinite(p[1])) s.points.push([p[0], p[1]]);
    }
    s.updated = performance.now();
    schedule();
  }

  function clearFrom(sender) {
    for (const [key, s] of strokes) if (s.sender === sender) strokes.delete(key);
    schedule();
  }

  // ---- Drawing ----
  let current; // { id, stream, rect, pending: [], timer }
  let nextId = 1;

  function flush() {
    if (!current) return;
    clearTimeout(current.timer);
    current.timer = undefined;
    if (!current.pending.length) return;
    send({ t: 'annotate', stroke: { id: current.id, stream: current.stream, color, points: current.pending } });
    current.pending = [];
  }

  function point(e) {
    const r = current.rect;
    const round = (v) => Math.round(Math.min(1, Math.max(0, v)) * 10000) / 10000;
    return [round((e.clientX - r.left) / r.width), round((e.clientY - r.top) / r.height)];
  }

  overlay.addEventListener('pointerdown', (e) => {
    if (!enabled) return;
    e.preventDefault();
    rects = undefined;
    const hit = [...streamRects()].find(([, r]) => e.clientX >= r.left && e.clientX <= r.left + r.width && e.clientY >= r.top && e.clientY <= r.top + r.height);
    if (!hit) return;
    overlay.setPointerCapture(e.pointerId);
    current = { id: `${session}-${nextId++}`, stream: hit[0], rect: hit[1], pending: [] };
    const p = point(e);
    current.pending.push(p);
    add(session, { id: current.id, stream: current.stream, color, points: [p] });
    flush();
  });
  overlay.addEventListener('pointermove', (e) => {
    if (!current) return;
    const events = e.getCoalescedEvents?.() ?? [e];
    const points = events.map(point);
    current.pending.push(...points);
    add(session, { id: current.id, stream: current.stream, color, points });
    current.timer ??= setTimeout(flush, SEND_EVERY_MS);
  });
  const end = () => {
    flush();
    current = undefined;
  };
  overlay.addEventListener('pointerup', end);
  overlay.addEventListener('pointercancel', end);

  function setEnabled(on) {
    if (enabled === on) return;
    enabled = on;
    if (!on) end();
    overlay.classList.toggle('drawing', on);
    palette.classList.toggle('hidden', !on);
    onChange(on);
  }

  sizeOverlay();
  return {
    get enabled() {
      return enabled;
    },
    setEnabled,
    toggle: () => setEnabled(!enabled),
    /** This viewer's session id (from welcome), so its own echoed strokes are skipped. */
    setSession(id) {
      session = id ?? 0;
    },
    onChange(fn) {
      onChange = fn;
    },
    /** The streams moved or changed size (layout, zoom). */
    invalidate,
    /** A host message: annotate { sender, stroke } or annotateClear { sender }. */
    onMessage(msg) {
      if (msg.sender === session && session) return;
      if (msg.t === 'annotate' && msg.stroke) add(msg.sender, msg.stroke);
      else if (msg.t === 'annotateClear') clearFrom(msg.sender);
    },
  };
}
