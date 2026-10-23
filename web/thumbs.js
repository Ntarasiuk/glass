// Display previews: a strip of small live pictures of each display along the bottom, the one
// being shown marked, tap one to switch. It sits in the empty band below the picture when
// there's room; otherwise it stays tucked behind a small tab (and opens over the picture).
// Hiding it is remembered.

const $ = (id) => document.getElementById(id);

const HIDDEN_KEY = 'glass.thumbsHidden';
/** How often previews are refreshed while the strip shows. */
const REFRESH_MS = 2000;
/** Room the strip needs below the picture to sit there without covering it. */
const ROOM_PX = 104;

function stored(key) {
  try { return localStorage.getItem(key); } catch { return null; }
}
function store(key, value) {
  try { localStorage.setItem(key, value); } catch {}
}

export function createThumbs({ control, onSelect, names, screens, pictures }) {
  const strip = $('thumbs');
  const list = $('thumbs-list');
  const tab = $('thumbs-show');
  const images = new Map(); // display id → <img>
  let displays = [];
  let streamed = [];
  let hidden = stored(HIDDEN_KEY) === '1';
  // Opened from the tab although there's no room: shown over the picture until hidden.
  let opened = false;
  let room = false;
  let connected = false;
  let timer;

  const multi = () => displays.length > 1;
  const showing = () => multi() && !hidden && (room || opened);

  function update() {
    const show = showing();
    strip.classList.toggle('hidden', !show);
    strip.classList.toggle('over', show && !room);
    tab.classList.toggle('hidden', !multi() || show);
    clearInterval(timer);
    timer = undefined;
    if (show && connected) {
      refresh();
      timer = setInterval(refresh, REFRESH_MS);
    }
  }

  function refresh() {
    if (document.visibilityState === 'visible') control({ t: 'thumbs' });
  }

  function render() {
    const all = streamed.length > 1;
    const label = names(displays);
    const ordered = [...displays].sort((a, b) => a.x - b.x || a.y - b.y);
    const items = ordered.map((d) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'thumb';
      button.dataset.id = d.id;
      const current = !all && streamed[0] === d.id;
      if (current) button.setAttribute('aria-current', 'true');
      const frame = document.createElement('span');
      frame.className = 'frame';
      frame.style.aspectRatio = `${d.width} / ${d.height}`;
      let img = images.get(d.id);
      if (!img) {
        img = document.createElement('img');
        img.alt = '';
        img.decoding = 'async';
        images.set(d.id, img);
      }
      frame.append(img);
      const name = document.createElement('span');
      name.className = 'name';
      name.textContent = label.get(d.id);
      button.append(frame, name);
      button.title = `${label.get(d.id)} · ${d.width}×${d.height}`;
      button.addEventListener('click', () => {
        if (!current) onSelect(d.id);
      });
      return button;
    });
    const allButton = document.createElement('button');
    allButton.type = 'button';
    allButton.className = 'thumb all';
    if (all) allButton.setAttribute('aria-current', 'true');
    allButton.innerHTML = '<span class="frame"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="6" width="9" height="12" rx="1.5"/><rect x="13" y="6" width="9" height="12" rx="1.5"/></svg></span><span class="name">All</span>';
    allButton.title = 'All displays, side by side';
    allButton.addEventListener('click', () => {
      if (!all) onSelect('all');
    });
    list.replaceChildren(...items, allButton);
  }

  /** Is there an empty band below the picture tall enough for the strip? */
  function measure() {
    let gap = Infinity;
    // Each shown stream: its element and the picture size it carries.
    for (const { el, w, h } of pictures()) {
      const r = el.getBoundingClientRect();
      if (!r.width || !r.height || !w || !h) continue;
      // object-fit: contain centres the picture in its box.
      const shown = Math.min(r.height, (r.width * h) / w);
      gap = Math.min(gap, (r.height - shown) / 2 + (innerHeight - r.bottom));
    }
    const next = gap !== Infinity && gap >= ROOM_PX;
    if (next !== room) {
      room = next;
      update();
    }
  }
  let measuring = 0;
  const queueMeasure = () => {
    measuring ||= requestAnimationFrame(() => {
      measuring = 0;
      measure();
    });
  };
  new ResizeObserver(queueMeasure).observe(screens);
  addEventListener('resize', queueMeasure, { passive: true });

  $('thumbs-hide').addEventListener('click', () => {
    hidden = true;
    opened = false;
    store(HIDDEN_KEY, '1');
    update();
  });
  tab.addEventListener('click', () => {
    hidden = false;
    opened = true;
    store(HIDDEN_KEY, '0');
    update();
  });
  document.addEventListener('visibilitychange', () => showing() && refresh());

  return {
    /** The host's displays, and which are being streamed (several = all). */
    setDisplays(next, nextStreamed) {
      displays = next;
      streamed = nextStreamed;
      for (const id of images.keys()) if (!displays.some((d) => d.id === id)) images.delete(id);
      render();
      update();
      queueMeasure();
    },
    /** A new picture arrived (the video size may have changed the room below it). */
    measure: queueMeasure,
    setConnected(on) {
      connected = on;
      update();
    },
    onMessage(msg) {
      for (const { id, png } of msg.thumbs ?? []) {
        const img = images.get(id);
        if (img) img.src = `data:image/png;base64,${png}`;
      }
    },
  };
}
