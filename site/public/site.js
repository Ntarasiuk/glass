// Shared by the site's pages: copy buttons, the feedback form and the glass.

import { lens } from '/glassy/lens.js';

for (const block of document.querySelectorAll('.command')) {
  const button = block.querySelector('.copy');
  button.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(block.dataset.copy);
      button.textContent = 'Copied';
    } catch {
      getSelection().selectAllChildren(block.querySelector('code'));
      button.textContent = 'Press ⌘C';
    }
    setTimeout(() => { button.textContent = 'Copy'; }, 1600);
  });
}

const form = document.getElementById('feedback-form');
if (form) {
  const status = document.getElementById('fb-status');
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const data = Object.fromEntries(new FormData(form));
    status.className = 'form-status';
    status.textContent = 'Sending…';
    try {
      const res = await fetch('/api/feedback', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ ...data, source: 'web' }),
      });
      const body = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(body.error || 'Something went wrong. Try again in a moment.');
      form.reset();
      status.className = 'form-status ok';
      status.textContent = 'Thank you, we got it.';
    } catch (err) {
      status.className = 'form-status err';
      status.textContent = err.message;
    }
  });
}

// The hero logo settles into place once; with reduced motion it stays on its final frame (the poster).
const mark = document.querySelector('video.mark');
if (mark && !matchMedia('(prefers-reduced-motion: reduce)').matches) mark.play().catch(() => {});

// Glass surfaces: Glassy UI's grain, and its edge lens where the browser can run it.
for (const surface of document.querySelectorAll('.lg-glass-surface')) {
  const noise = document.createElement('span');
  noise.className = 'lg-noise';
  noise.ariaHidden = 'true';
  surface.prepend(noise);
  lens(surface);
}
