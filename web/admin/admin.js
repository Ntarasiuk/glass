// The owner's console: talks to /admin/api/* (owner-only; this page's cookie or a tailnet
// device proves it). Plain DOM, no build step.

const $ = (id) => document.getElementById(id);

async function api(path, { method = 'GET', body } = {}) {
  const r = await fetch(`/admin/api/${path}`, {
    method,
    headers: body ? { 'content-type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  });
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw Object.assign(new Error(data.error ?? `HTTP ${r.status}`), { status: r.status });
  return data;
}

let toastTimer;
function toast(text) {
  const t = $('toast');
  t.textContent = text;
  t.classList.remove('hidden');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.add('hidden'), 3500);
}

function el(tag, props = {}, ...children) {
  const e = document.createElement(tag);
  Object.assign(e, props);
  e.append(...children.filter((c) => c !== undefined && c !== null));
  return e;
}

function row(text, tag, ...actions) {
  return el('li', {}, el('span', { className: 'grow', textContent: text }), tag ? el('span', { className: 'tag', textContent: tag }) : undefined, ...actions);
}

function button(label, onClick, className = '') {
  const b = el('button', { type: 'button', textContent: label, className });
  b.addEventListener('click', async () => {
    try {
      await onClick();
    } catch (e) {
      toast(e.message);
    }
  });
  return b;
}

function empty(list, text) {
  if (!list.children.length) list.append(el('li', { className: 'dim', textContent: text }));
}

const ago = (unix) => {
  const s = Math.max(0, Date.now() / 1000 - unix);
  return s < 3600 ? `${Math.round(s / 60)} min` : s < 86400 ? `${Math.round(s / 3600)} h` : `${Math.round(s / 86400)} d`;
};

function scopeText(s) {
  const parts = [s.view_only && 'view only', !s.files && 'no files', !s.clipboard && 'no clipboard', s.ask && 'asks first', s.passkey && 'passkey'].filter(Boolean);
  return parts.length ? parts.join(', ') : 'full control';
}

async function refresh() {
  let state;
  try {
    state = await api('state');
  } catch (e) {
    if (e.status === 403) {
      $('denied').classList.remove('hidden');
      $('console').classList.add('hidden');
      return;
    }
    throw e;
  }
  $('denied').classList.add('hidden');
  $('console').classList.remove('hidden');
  $('version').textContent = `Glass ${state.version}`;

  const viewers = $('viewers');
  viewers.replaceChildren(...state.viewers.map((v) => row(v.describe, '', button('Disconnect', async () => {
    await api('disconnect', { method: 'POST', body: { id: v.id } });
    refresh();
  }, 'danger'))));
  empty(viewers, 'Nobody is connected.');

  const pending = $('pending');
  pending.replaceChildren(...state.pending.map((p) => row(p.who, '', button('Allow', async () => {
    await api('decide', { method: 'POST', body: { id: p.id, allow: true } });
    refresh();
  }, 'primary'), button('Deny', async () => {
    await api('decide', { method: 'POST', body: { id: p.id, allow: false } });
    refresh();
  }, 'danger'))));
  empty(pending, 'Nobody is waiting.');

  const now = Date.now() / 1000;
  const passes = $('passes');
  passes.replaceChildren(...state.passes.map((p) => row(
    `${p.device || 'A browser'} · ${scopeText(p.scope)}`,
    `${p.id} · ${ago(2 * now - p.expires)} left`,
    button('Revoke', async () => {
      await api('revoke', { method: 'POST', body: { id: p.id } });
      toast('Revoked.');
      refresh();
    }, 'danger'),
  )));
  empty(passes, 'No devices have link access.');

  const settings = $('settings');
  settings.replaceChildren(...state.settings.map((s) => {
    const onOff = s.value === 'on' || s.value === 'off';
    const input = onOff
      ? el('input', { type: 'checkbox', checked: s.value === 'on' })
      : el('input', { value: s.key === 'oidc-client-secret' ? '' : s.value, placeholder: s.key === 'oidc-client-secret' && s.value ? '(set)' : '' });
    input.setAttribute('aria-label', s.key);
    input.dataset.key = s.key;
    const save = async () => {
      const value = onOff ? (input.checked ? 'on' : 'off') : input.value;
      try {
        const r = await api('settings', { method: 'PUT', body: { key: s.key, value } });
        toast(`${r.key} = ${r.value || '(empty)'}`);
      } catch (e) {
        toast(e.message);
        refresh();
      }
    };
    input.addEventListener('change', save);
    return el('tr', { title: s.help }, el('td', { className: 'key', textContent: s.key }), el('td', {}, input));
  }));

  $('log').textContent = state.log.map((l) => l.describe).join('\n') || 'No connections yet.';
  loadMachines();
  loadRules();
}

// ---- Links ----

$('link-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const f = new FormData(e.target);
  try {
    const made = await api('link', {
      method: 'POST',
      body: {
        duration: f.get('duration'),
        anywhere: f.has('anywhere'),
        scope: { view_only: f.has('view_only'), files: !f.has('no_files'), clipboard: !f.has('no_clipboard'), ask: f.has('ask'), passkey: f.has('passkey') },
      },
    });
    $('qr').innerHTML = made.qr;
    $('made-url').textContent = made.url;
    $('made-summary').textContent = made.summary;
    $('made').classList.remove('hidden');
    navigator.clipboard?.writeText(made.url).catch(() => {});
  } catch (err) {
    toast(err.message);
  }
});

// ---- Address book ----

let groups = [];
async function saveGroups(next) {
  const saved = await api('groups', { method: 'PUT', body: { groups: next } });
  groups = saved.groups;
  loadMachines();
}

async function loadMachines() {
  const { machines, groups: g } = await api('machines');
  groups = g;
  const box = $('machines');
  const sections = [...groups.map((x) => x.name), null];
  const out = [];
  for (const name of sections) {
    const members = machines.filter((m) => (m.group ?? null) === name);
    if (name === null && !members.length) continue;
    const header = el('h3', { textContent: name ?? 'Not in a group' });
    if (name !== null) {
      header.append(' ', button('Rename', async () => {
        const to = prompt('Rename group', name)?.trim();
        if (to) await saveGroups(groups.map((x) => (x.name === name ? { ...x, name: to } : x)));
      }), ' ', button('Delete', async () => {
        await saveGroups(groups.filter((x) => x.name !== name));
      }, 'danger'));
    }
    const list = el('ul', { className: 'rows' });
    for (const m of members) {
      const select = el('select', {}, el('option', { value: '', textContent: 'No group' }), ...groups.map((x) => el('option', { value: x.name, textContent: x.name, selected: x.name === m.group })));
      select.setAttribute('aria-label', `Group for ${m.name}`);
      select.addEventListener('change', () => {
        const dns = m.dns.toLowerCase();
        saveGroups(groups.map((x) => ({
          ...x,
          machines: x.name === select.value ? [...x.machines.filter((d) => d !== dns), dns] : x.machines.filter((d) => d !== dns),
        }))).catch((e) => toast(e.message));
      });
      const actions = [select];
      if (m.online && !m.self) actions.push(el('a', { className: 'button', href: `https://${m.dns}/`, target: '_blank', rel: 'noopener', textContent: 'Open' }));
      if (!m.online && m.macs?.length) {
        actions.push(button('Wake', async () => {
          const r = await api('wake', { method: 'POST', body: { target: m.dns } });
          toast(`Wake-up sent (${r.sent}).`);
        }));
      }
      list.append(row(m.name, `${m.self ? 'this computer' : m.online ? 'online' : 'offline'} · ${m.os}`, ...actions));
    }
    empty(list, 'No machines in this group yet.');
    out.push(header, list);
  }
  if (!machines.length) out.push(el('p', { className: 'dim', textContent: 'No tailnet machines found (is Tailscale running?).' }));
  box.replaceChildren(...out);
}

$('group-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const name = new FormData(e.target).get('name').trim();
  if (!name) return;
  try {
    await saveGroups([...groups, { name, machines: [] }]);
    e.target.reset();
  } catch (err) {
    toast(err.message);
  }
});

// ---- Access rules (acl.rs) ----

let rules = null;
async function loadRules() {
  const list = $('acl');
  try {
    rules = (await api('acl')).rules ?? [];
  } catch (e) {
    rules = null;
    list.replaceChildren(el('li', { className: 'dim', textContent: e.status === 404 ? 'Access rules are unavailable on this version of Glass.' : e.message }));
    $('acl-form').classList.add('hidden');
    return;
  }
  $('acl-form').classList.remove('hidden');
  list.replaceChildren(...rules.map((r, i) => row(r.match, scopeText({ files: true, clipboard: true, ...r.scope }), button('Remove', async () => {
    await saveRules(rules.filter((_, j) => j !== i));
  }, 'danger'))));
  empty(list, 'No rules: single sign-on uses its default scope.');
}

async function saveRules(next) {
  $('acl-error').classList.add('hidden');
  try {
    await api('acl', { method: 'PUT', body: { rules: next } });
  } catch (e) {
    $('acl-error').textContent = e.message;
    $('acl-error').classList.remove('hidden');
  }
  loadRules();
}

$('acl-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  if (!rules) return;
  const f = new FormData(e.target);
  const match = f.get('match').trim();
  if (!match) return;
  await saveRules([...rules, { match, scope: { view_only: f.has('view_only'), files: !f.has('no_files'), clipboard: true, ask: f.has('ask'), passkey: false } }]);
  e.target.reset();
});

refresh().catch((e) => toast(e.message));
// Who's connected changes on its own; keep it fresh while the page is open.
// Not while typing: a redraw would throw away an unsaved field.
setInterval(() => {
  const typing = document.activeElement?.matches?.('input, select');
  if (!document.hidden && !typing) refresh().catch(() => {});
}, 5000);
