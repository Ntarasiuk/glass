// What the computer is playing reaches the browser's media controls (test mode plays a
// fixed track).

import { Host, expectVideo, test, expect } from './harness.mjs';

test('the media controls show what the computer is playing', async ({ browser }) => {
  const host = await new Host().start();
  try {
    const page = await (await browser.newContext()).newPage();
    await page.goto(host.url());
    await expectVideo(page);
    await expect
      .poll(() => page.evaluate(() => navigator.mediaSession.metadata && { title: navigator.mediaSession.metadata.title, artist: navigator.mediaSession.metadata.artist, album: navigator.mediaSession.metadata.album, art: navigator.mediaSession.metadata.artwork[0]?.src.startsWith('blob:') }))
      .toEqual({ title: 'Gravity', artist: 'John Mayer', album: 'Continuum', art: true });
    expect(await page.evaluate(() => navigator.mediaSession.playbackState)).toBe('playing');
  } finally {
    await host.stop();
  }
});
