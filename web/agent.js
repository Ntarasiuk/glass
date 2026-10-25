// An agent using this computer (glass mcp): a banner while it works, with Pause and Stop
// for the owner and its recent steps; a marker where it points; and its questions (help,
// or approve an action) as cards to answer.

const $ = (id) => document.getElementById(id);

/** The banner stays this long after the agent's last step. */
const IDLE_MS = 30_000;
const RECENT = 8;

export function createAgent({ control, locate }) {
  const bar = $('agent-bar');
  const label = $('agent-label');
  const recentList = $('agent-recent');
  const pauseButton = $('agent-pause');
  const stopButton = $('agent-stop');
  const asks = $('agent-asks');
  const marker = $('agent-marker');
  let status = { paused: false, stopped: false };
  let owner = false;
  let lastAction = 0;
  let idleTimer;
  const recent = [];

  function render() {
    const working = Date.now() - lastAction < IDLE_MS;
    const show = working || status.paused || status.stopped || asks.childElementCount > 0;
    bar.classList.toggle('hidden', !show);
    bar.dataset.state = status.stopped ? 'stopped' : status.paused ? 'paused' : 'working';
    const last = recent[0];
    label.textContent = status.stopped
      ? 'Agent stopped'
      : status.paused
        ? 'Agent paused'
        : last ? `${last.agent ?? 'Agent'} · ${last.kind} ${last.detail}` : 'Agent working';
    pauseButton.textContent = status.paused ? 'Resume' : 'Pause';
    pauseButton.classList.toggle('hidden', !owner || status.stopped);
    stopButton.textContent = status.stopped ? 'Allow again' : 'Stop';
    stopButton.classList.toggle('hidden', !owner);
    recentList.replaceChildren(
      ...recent.map((a) => {
        const li = document.createElement('li');
        li.textContent = `${a.kind} ${a.detail}`;
        return li;
      }),
    );
    clearTimeout(idleTimer);
    if (working) idleTimer = setTimeout(render, IDLE_MS - (Date.now() - lastAction) + 50);
  }

  pauseButton.addEventListener('click', () => control({ t: 'agent', op: status.paused ? 'resume' : 'pause' }));
  stopButton.addEventListener('click', () => control({ t: 'agent', op: status.stopped ? 'allow' : 'stop' }));

  function showMarker(display, x, y) {
    const at = locate(display, x, y);
    if (!at) return;
    marker.style.translate = `${at.x}px ${at.y}px`;
    marker.classList.remove('hidden');
    // Restart the pulse.
    marker.classList.remove('pulse');
    void marker.offsetWidth;
    marker.classList.add('pulse');
  }
  marker.addEventListener('animationend', () => marker.classList.add('hidden'));

  function addAsk({ id, kind, text, choices }) {
    if (asks.querySelector(`[data-id="${id}"]`)) return;
    const card = document.createElement('div');
    card.className = 'agent-ask glass';
    card.dataset.id = id;
    card.setAttribute('role', 'alertdialog');
    const title = document.createElement('strong');
    title.textContent = kind === 'help'
      ? 'The agent needs your help'
      : kind === 'consent' ? 'An agent asks to use this computer' : 'The agent asks before it acts';
    const body = document.createElement('p');
    // A consent's text ends with its request id, which isn't for people.
    body.textContent = kind === 'consent' ? text.replace(/ \[[^\]]+\]$/, '') : text;
    card.append(title, body);
    if (owner && kind === 'consent') {
      // Number matching: the right one is on the agent's sign-in page.
      const row = document.createElement('div');
      row.className = 'row choices';
      const answer = (ok, note) => {
        control({ t: 'agentAnswer', id, ok, note });
        card.remove();
        render();
      };
      for (const n of choices ?? []) {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'choice';
        b.textContent = n;
        b.addEventListener('click', () => answer(true, n));
        row.append(b);
      }
      const no = document.createElement('button');
      no.type = 'button';
      no.textContent = 'Deny';
      no.addEventListener('click', () => answer(false, null));
      row.append(no);
      card.append(row);
    } else if (owner) {
      const note = document.createElement('input');
      note.type = 'text';
      note.placeholder = 'Note for the agent (optional)';
      note.setAttribute('aria-label', 'Note for the agent');
      const row = document.createElement('div');
      row.className = 'row';
      const yes = document.createElement('button');
      yes.type = 'button';
      yes.className = 'primary';
      yes.textContent = kind === 'help' ? 'Done, hand back' : 'Allow';
      const no = document.createElement('button');
      no.type = 'button';
      no.textContent = kind === 'help' ? "Can't" : "Don't";
      const answer = (ok) => {
        control({ t: 'agentAnswer', id, ok, note: note.value.trim() || null });
        card.remove();
        render();
      };
      yes.addEventListener('click', () => answer(true));
      no.addEventListener('click', () => answer(false));
      note.addEventListener('keydown', (e) => {
        e.stopPropagation();
        if (e.key === 'Enter') answer(true);
      });
      note.addEventListener('keyup', (e) => e.stopPropagation());
      row.append(no, yes);
      card.append(note, row);
    } else {
      const wait = document.createElement('p');
      wait.className = 'dim';
      wait.textContent = "Waiting for the computer's owner to answer.";
      card.append(wait);
    }
    asks.append(card);
    render();
  }

  return {
    setOwner(on) {
      owner = on;
      render();
    },
    onMessage(msg) {
      switch (msg.t) {
        case 'agentStatus':
          status = msg.status;
          break;
        case 'agentAction':
          lastAction = Date.now();
          recent.unshift({ agent: msg.agent, kind: msg.kind, detail: msg.detail });
          recent.length = Math.min(recent.length, RECENT);
          if (msg.display != null && msg.x != null) showMarker(msg.display, msg.x, msg.y);
          break;
        case 'agentAsk':
          addAsk(msg);
          break;
        case 'agentAnswered':
          asks.querySelector(`[data-id="${msg.id}"]`)?.remove();
          break;
      }
      render();
    },
    /** A new session: the host sends the current status and questions again. */
    reset() {
      status = { paused: false, stopped: false };
      asks.replaceChildren();
      render();
    },
  };
}
