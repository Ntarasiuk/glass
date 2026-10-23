// Toolbar, panels, toasts and the token prompt. DOM writes only; no layout reads.

const $ = (id) => document.getElementById(id);

const TOOLBAR_IDLE_MS = 3000;
// Matches the slide-in transition in index.html (420 ms), plus a beat.
const TOOLBAR_WAKE_MS = 450;

export function createUI({ onOpenMachines, onSelectMachine, onSelectDisplay, onShortcut, shortcuts, swapCmdOption, onSwapCmdOption, ctrlAsCmd, onCtrlAsCmd, pcKeyboard, onPcKeyboard, onKeyboard, onUploadFiles, onRequestFiles, onAudio, onMic, onFullscreen, onToggleStats, onToken, resolution = 'auto', onResolution, fitSizes = () => [], deviceName = () => 'device' }) {
  const toolbar = $('toolbar');
  // Each toolbar menu: its button and panel.
  const menus = {
    machines: { button: $('btn-machines'), panel: $('machines-panel') },
    displays: { button: $('btn-displays'), panel: $('displays-panel') },
    shortcuts: { button: $('btn-shortcuts'), panel: $('shortcuts-panel') },
  };
  const fileInput = $('file-input');
  const toasts = $('toasts');
  const auth = $('auth');

  // Fade the toolbar when idle so it doesn't cover the remote screen.
  let idleTimer;
  let wakingTimer;
  function wakeToolbar() {
    if (toolbar.classList.contains('idle')) {
      // Buttons ignore presses until it has finished sliding in, so a tap or click aimed
      // at the tucked toolbar can't land on whichever button slides under it.
      toolbar.classList.add('waking');
      clearTimeout(wakingTimer);
      wakingTimer = setTimeout(() => toolbar.classList.remove('waking'), TOOLBAR_WAKE_MS);
    }
    toolbar.classList.remove('idle');
    clearTimeout(idleTimer);
    idleTimer = setTimeout(() => {
      // Stay put while one of its menus is open.
      const menuOpen = document.querySelector('.panel:not(.hidden)');
      if (menuOpen) wakeToolbar();
      else toolbar.classList.add('idle');
    }, TOOLBAR_IDLE_MS);
  }
  toolbar.addEventListener('pointerenter', wakeToolbar);
  toolbar.addEventListener('pointerdown', wakeToolbar);
  toolbar.addEventListener('focusin', wakeToolbar);
  // A mouse heading for the top edge brings it back before it's reached.
  addEventListener('pointermove', (e) => {
    if (e.pointerType === 'mouse' && e.clientY < 72 && toolbar.classList.contains('idle')) wakeToolbar();
  }, { passive: true });
  // Fingers can't hover, so a tucked toolbar shows a grab handle sized for a fingertip
  // (CSS keys it off data-input). A tap on the tucked toolbar only wakes it.
  const root = document.documentElement;
  root.dataset.input = matchMedia('(hover: none)').matches ? 'touch' : 'mouse';
  addEventListener('pointerdown', (e) => {
    const input = e.pointerType === 'touch' ? 'touch' : 'mouse';
    if (root.dataset.input !== input) root.dataset.input = input;
  }, { capture: true, passive: true });
  wakeToolbar();

  // Toolbar buttons must not steal pointer events from the canvas logic or keep focus.
  const on = (id, fn) => $(id).addEventListener('click', (e) => { e.preventDefault(); wakeToolbar(); fn(e); });

  on('btn-machines', () => {
    toggleMenu('machines');
    if (!menus.machines.panel.classList.contains('hidden')) onOpenMachines();
  });
  on('btn-displays', () => toggleMenu('displays'));
  on('btn-shortcuts', () => toggleMenu('shortcuts'));
  on('btn-keyboard', onKeyboard);
  on('btn-upload', () => fileInput.click());
  on('btn-download', onRequestFiles);
  on('btn-audio', onAudio);
  on('btn-mic', () => onMic?.());
  on('btn-fullscreen', onFullscreen);
  on('btn-stats', onToggleStats);

  fileInput.addEventListener('change', () => {
    if (fileInput.files.length) onUploadFiles([...fileInput.files]);
    fileInput.value = '';
  });

  // Drag-and-drop files from the client device onto the remote screen.
  addEventListener('dragover', (e) => e.preventDefault());
  addEventListener('drop', (e) => {
    e.preventDefault();
    const files = [...(e.dataTransfer?.files ?? [])];
    if (files.length) onUploadFiles(files);
  });

  addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    const open = Object.keys(menus).find((name) => !menus[name].panel.classList.contains('hidden'));
    if (!open) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    setMenuOpen(open, false);
    menus[open].button.focus();
  }, { capture: true });

  function setMenuOpen(name, open) {
    const { button, panel } = menus[name];
    panel.classList.toggle('hidden', !open);
    button.setAttribute('aria-expanded', String(open));
  }
  function toggleMenu(name) {
    for (const other of Object.keys(menus)) {
      setMenuOpen(other, other === name && menus[name].panel.classList.contains('hidden'));
    }
  }
  addEventListener('pointerdown', (e) => {
    for (const [name, { button, panel }] of Object.entries(menus)) {
      if (!panel.contains(e.target) && !button.contains(e.target)) setMenuOpen(name, false);
    }
  }, { capture: true });

  function menuItem(label, meta, { checked, onSelect }) {
    const b = document.createElement('button');
    b.setAttribute('role', checked === undefined ? 'menuitem' : 'menuitemradio');
    if (checked !== undefined) b.setAttribute('aria-checked', String(checked));
    const l = document.createElement('span');
    l.textContent = label;
    const m = document.createElement('span');
    m.className = 'meta';
    m.textContent = meta;
    b.append(l, m);
    b.addEventListener('click', onSelect);
    return b;
  }

  function toggleItem(label, meta, initial, onChange) {
    let on = initial;
    const item = menuItem(label, meta, {
      checked: on,
      onSelect: () => {
        on = !on;
        item.setAttribute('aria-checked', String(on));
        onChange(on);
      },
    });
    return item;
  }
  const pcItem = toggleItem('PC keyboard', 'Ctrl → ⌘, Super → ⌃', pcKeyboard, onPcKeyboard);
  const ctrlItem = toggleItem('Ctrl as ⌘', '⌃⇥ ⌃Space ⌃H ⌃⇧3–5', ctrlAsCmd, onCtrlAsCmd);

  let swapOn = swapCmdOption;
  const swapItem = menuItem('Swap ⌘ and ⌥', 'see note', {
    checked: swapOn,
    onSelect: () => {
      swapOn = !swapOn;
      swapItem.setAttribute('aria-checked', String(swapOn));
      onSwapCmdOption(swapOn);
    },
  });
  swapItem.title = 'Pair with iPad Settings › General › Keyboard › Hardware Keyboard › Modifier Keys: Command → Option, Option → Command. Lets ⌘Tab, ⌘Space and ⌘H reach the Mac.';
  const note = document.createElement('p');
  note.className = 'note';
  note.textContent = 'iPadOS keeps ⌘Tab, ⌘Space, ⌘H and ⌘⇧3–5. “Ctrl as ⌘” sends those combos when you press Ctrl instead. Alternatively swap Command and Option in iPad Settings › Keyboard › Hardware Keyboard › Modifier Keys and turn on “Swap ⌘ and ⌥”.';
  const divider = document.createElement('hr');

  menus.shortcuts.panel.replaceChildren(
    pcItem,
    ctrlItem,
    swapItem,
    note,
    divider,
    ...shortcuts.map((sc) =>
      menuItem(sc.label, sc.keysLabel, {
        onSelect: () => {
          setMenuOpen('shortcuts', false);
          onShortcut(sc);
        },
      }),
    ),
  );

  /** `current` is the id ("host" or "host:port") of the machine this session is on. */
  function renderMachines(machines, current) {
    if (!machines) {
      menus.machines.panel.replaceChildren(menuItem('Looking for machines…', '', { onSelect: () => {} }));
      return;
    }
    const status = (m) => (m.id === current ? 'connected' : !m.online ? 'offline' : !m.glass ? 'Glass not running' : m.os);
    const items = machines.map((m) => {
      const item = menuItem(m.name, status(m), {
        checked: m.id === current,
        onSelect: () => {
          setMenuOpen('machines', false);
          if (m.id !== current) onSelectMachine(m);
        },
      });
      item.disabled = m.id !== current && (!m.online || !m.glass);
      return item;
    });
    if (!items.length) items.push(menuItem('No machines found', 'needs Tailscale', { onSelect: () => {} }));
    menus.machines.panel.replaceChildren(...items);
  }

  /** `streamed` lists the display ids currently shown, in stream order. */
  function renderDisplays(displays, streamed) {
    const all = streamed.length > 1;
    const names = positionNames(displays);
    const ordered = [...displays].sort((a, b) => a.x - b.x || a.y - b.y);
    const items = ordered.map((d) =>
      menuItem(names.get(d.id), `${d.width}×${d.height}${d.main ? ' · main' : ''}`, {
        checked: !all && streamed[0] === d.id,
        onSelect: () => {
          setMenuOpen('displays', false);
          if (all || streamed[0] !== d.id) onSelectDisplay(d.id);
        },
      }),
    );
    if (displays.length > 1) {
      items.push(
        menuItem('All displays', `${displays.length} side by side`, {
          checked: all,
          onSelect: () => {
            setMenuOpen('displays', false);
            if (!all) onSelectDisplay('all');
          },
        }),
      );
    }
    // Resolution: sized to this screen (sharper and faster), or the Mac's full size.
    const native = displays.find((d) => d.id === streamed[0]) ?? displays[0];
    items.push(
      document.createElement('hr'),
      menuItem('Auto', 'sized to this screen', {
        checked: resolutionMode === 'auto',
        onSelect: () => { setMenuOpen('displays', false); onResolution?.('auto'); },
      }),
      menuItem('This Mac', native ? `${native.width}×${native.height}` : 'full resolution', {
        checked: resolutionMode === 'native',
        onSelect: () => { setMenuOpen('displays', false); onResolution?.('native'); },
      }),
    );
    // Fit this device: the Mac's desktop in this screen's shape, no black bars.
    const fits = canFit ? fitSizes() : [];
    if (fits.length) {
      const heading = document.createElement('p');
      heading.className = 'note';
      heading.textContent = `Fit this ${deviceName()} · changes the Mac's display shape while you're connected`;
      items.push(document.createElement('hr'), heading);
      for (const [w, h] of fits) {
        const mode = `fit:${w}x${h}`;
        items.push(menuItem(`${w}×${h}`, '', {
          checked: resolutionMode === mode,
          onSelect: () => { setMenuOpen('displays', false); onResolution?.(mode); },
        }));
      }
    }
    lastDisplays = [displays, streamed];
    menus.displays.panel.replaceChildren(...items);
  }
  let resolutionMode = resolution;
  let lastDisplays;
  let canFit = true;

  /** Names displays by where they sit in the macOS arrangement: Left / Right, Top / Bottom. */
  function positionNames(displays) {
    const horizontal =
      Math.max(...displays.map((d) => d.x)) - Math.min(...displays.map((d) => d.x)) >=
      Math.max(...displays.map((d) => d.y)) - Math.min(...displays.map((d) => d.y));
    const key = horizontal ? 'x' : 'y';
    const ordered = [...displays].sort((a, b) => a[key] - b[key]);
    const words = horizontal ? ['Left', 'Middle', 'Right'] : ['Top', 'Middle', 'Bottom'];
    const names = new Map();
    ordered.forEach((d, i) => {
      let name;
      if (displays.length === 1) name = d.builtin ? 'Built-in display' : 'Display';
      else if (displays.length === 2) name = `${words[i === 0 ? 0 : 2]} display`;
      else if (displays.length === 3) name = `${words[i]} display`;
      else name = `Display ${i + 1}`;
      names.set(d.id, name);
    });
    return names;
  }

  const ICONS = {
    link: '<path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7"/><path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7"/>',
    info: '<path d="M12 16v-4M12 8h.01"/><circle cx="12" cy="12" r="9"/>',
    error: '<path d="M12 8v4M12 16h.01"/><path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/>',
    file: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>',
    update: '<path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/>',
  };
  const svg = (paths, cls) =>
    `<svg class="${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths}</svg>`;

  function toast({ text, kind, icon, action, timeout = 4000, progress }) {
    const el = document.createElement('div');
    el.className = `toast glass${kind === 'error' ? ' error' : ''}`;
    el.setAttribute('role', kind === 'error' ? 'alert' : 'status');
    el.insertAdjacentHTML('afterbegin', svg(ICONS[icon ?? (kind === 'error' ? 'error' : 'info')], 'icon'));
    const label = document.createElement('span');
    label.className = 'text';
    label.textContent = text;
    label.title = text;
    el.append(label);
    let bar;
    if (progress !== undefined) {
      bar = document.createElement('progress');
      bar.max = 1;
      bar.value = progress;
      el.append(bar);
    }
    let timer;
    const remove = () => {
      clearTimeout(timer);
      el.classList.add('leaving');
      setTimeout(() => el.remove(), 180);
    };
    if (action) {
      const b = document.createElement('button');
      b.className = 'action';
      b.textContent = action.label;
      b.addEventListener('click', () => { action.run(); remove(); });
      el.append(b);
    }
    const close = document.createElement('button');
    close.className = 'close';
    close.setAttribute('aria-label', 'Dismiss');
    close.innerHTML = svg('<path d="M6 6l12 12M18 6 6 18"/>', '');
    close.addEventListener('click', remove);
    el.append(close);
    toasts.append(el);
    const api = {
      update({ text: t, progress: p }) {
        if (t !== undefined) label.textContent = label.title = t;
        if (bar && p !== undefined) bar.value = p;
      },
      dismiss(after = 0) {
        clearTimeout(timer);
        timer = setTimeout(remove, after);
      },
    };
    if (timeout) api.dismiss(timeout);
    return api;
  }

  auth.querySelector('form').addEventListener('submit', (e) => {
    e.preventDefault();
    const token = new FormData(e.target).get('token').trim();
    if (!token) return;
    auth.classList.add('hidden');
    onToken(token);
  });

  return {
    renderMachines,
    renderDisplays,
    toast,
    /** `rejected`: a token was tried and refused, so say so. */
    showAuth({ rejected = false } = {}) {
      auth.classList.remove('hidden');
      $('auth-error').classList.toggle('hidden', !rejected);
      const input = auth.querySelector('input');
      input.setAttribute('aria-invalid', String(rejected));
      if (rejected) input.select();
      input.focus();
    },
    /** Guests (signed in with a magic link) can't switch machines. */
    setGuest(guest) {
      $('btn-machines').classList.toggle('hidden', guest);
      if (guest) setMenuOpen('machines', false);
    },
    /** Only the owner can reshape the host's display (guests get an error). */
    setCanFit(on) {
      canFit = on;
      if (lastDisplays) renderDisplays(...lastDisplays);
    },
    setResolution(mode) {
      resolutionMode = mode;
      if (lastDisplays) renderDisplays(...lastDisplays);
    },
    setStatsPressed(on) {
      $('btn-stats').setAttribute('aria-pressed', String(on));
    },
    /** The ⌘ menu (Mac shortcuts and key remaps) only applies to Mac hosts. */
    setHostOs(os) {
      $('btn-shortcuts').classList.toggle('hidden', os !== 'macos');
    },
    setAudioAvailable(available) {
      $('btn-audio').classList.toggle('hidden', !available);
    },
    setAudioOn(on) {
      $('audio-waves').style.opacity = on ? '1' : '0.25';
    },
    setMicAvailable(available) {
      $('btn-mic').classList.toggle('hidden', !available);
    },
    setMicOn(on) {
      $('btn-mic').setAttribute('aria-pressed', String(on));
    },
  };
}
