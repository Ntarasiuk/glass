// Chat between viewers and the person at the computer: a toolbar button with an unread
// dot and a small panel. Messages from others also pop up as toasts while it's closed.

const MAX_MESSAGES = 200;

export function createChat({ onSend, toast }) {
  const toolbar = document.getElementById('toolbar');
  const button = document.createElement('button');
  button.id = 'btn-chat';
  button.title = 'Chat';
  button.setAttribute('aria-haspopup', 'true');
  button.setAttribute('aria-expanded', 'false');
  button.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a8 8 0 0 1-11.6 7.1L4 20.5l1.4-4.6A8 8 0 1 1 21 12z"/></svg><span class="dot hidden"></span>';
  // With drawing and recording: things for everyone connected.
  document.getElementById('tb-together').prepend(button);

  const panel = document.createElement('div');
  panel.id = 'chat-panel';
  panel.className = 'panel glass hidden';
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-label', 'Chat');
  const list = document.createElement('ol');
  list.className = 'chat-log';
  const empty = document.createElement('p');
  empty.className = 'note';
  empty.textContent = 'Messages go to the person at this computer and anyone else viewing.';
  const form = document.createElement('form');
  form.className = 'chat-form';
  const field = document.createElement('input');
  field.name = 'text';
  field.autocomplete = 'off';
  field.placeholder = 'Message';
  field.setAttribute('aria-label', 'Message');
  field.maxLength = 2000;
  const sendButton = document.createElement('button');
  sendButton.type = 'submit';
  sendButton.textContent = 'Send';
  form.append(field, sendButton);
  panel.append(empty, list, form);
  document.body.append(panel);

  const dot = button.querySelector('.dot');
  const isOpen = () => !panel.classList.contains('hidden');
  function setOpen(open) {
    panel.classList.toggle('hidden', !open);
    button.setAttribute('aria-expanded', String(open));
    if (open) {
      dot.classList.add('hidden');
      field.focus();
      list.lastElementChild?.scrollIntoView({ block: 'end' });
    }
  }
  button.addEventListener('click', (e) => {
    e.preventDefault();
    setOpen(!isOpen());
  });
  addEventListener('pointerdown', (e) => {
    if (isOpen() && !panel.contains(e.target) && !button.contains(e.target)) setOpen(false);
  }, { capture: true });
  // Typing here is for the chat, not the remote keyboard.
  for (const type of ['keydown', 'keyup', 'keypress']) {
    panel.addEventListener(type, (e) => {
      if (e.key === 'Escape' && type === 'keydown') {
        setOpen(false);
        button.focus();
      }
      e.stopPropagation();
    });
  }
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const text = field.value.trim();
    if (!text) return;
    onSend(text);
    add({ from: 'You', text, mine: true });
    field.value = '';
  });

  function add({ from, text, mine = false }) {
    empty.classList.add('hidden');
    const item = document.createElement('li');
    item.className = mine ? 'mine' : '';
    const who = document.createElement('b');
    who.textContent = from;
    const body = document.createElement('span');
    body.textContent = text;
    item.append(who, body);
    list.append(item);
    while (list.children.length > MAX_MESSAGES) list.firstElementChild.remove();
    if (isOpen()) item.scrollIntoView({ block: 'end' });
  }

  return {
    /** A message from the host or another viewer. */
    receive({ from, text }) {
      add({ from, text });
      if (!isOpen()) {
        dot.classList.remove('hidden');
        toast({ text: `${from}: ${text}`, icon: 'chat', timeout: 6000, action: { label: 'Reply', run: () => setOpen(true) } });
      }
    },
    setAvailable(on) {
      button.classList.toggle('hidden', !on);
      if (!on) setOpen(false);
    },
    open() {
      setOpen(true);
    },
  };
}
