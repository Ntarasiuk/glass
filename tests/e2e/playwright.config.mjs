import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.mjs',
  timeout: 60_000,
  fullyParallel: true,
  // Every test runs a host encoding video plus a Chrome decoding it: more than two at once
  // starves a busy laptop into timeouts. PW_WORKERS overrides.
  workers: Number(process.env.PW_WORKERS ?? 2),
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['github']] : 'list',
  use: {
    // Google Chrome, not bundled Chromium: WebCodecs H.264 needs the proprietary codecs.
    channel: 'chrome',
    headless: true,
    viewport: { width: 1280, height: 720 },
    launchOptions: { args: ['--autoplay-policy=no-user-gesture-required'] },
    trace: 'retain-on-failure',
  },
});
