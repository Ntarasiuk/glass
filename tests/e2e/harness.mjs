// Starts isolated `glass --test` hosts and gives tests helpers for the page and the
// host's /test/* hooks. Each host gets its own ports, config dir, downloads and logs.

import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { expect } from '@playwright/test';

const ROOT = resolve(import.meta.dirname, '../..');
export const BINARY = process.env.GLASS_BIN ?? join(ROOT, 'target/release/glass-host');

let nextPort = 21000 + (Number(process.env.TEST_PARALLEL_INDEX ?? 0) * 20);

export class Host {
  /** `os`: what the host reports itself as (macos or linux). */
  constructor({ configDir, os = 'macos' } = {}) {
    this.os = os;
    this.dir = mkdtempSync(join(tmpdir(), 'glass-e2e-'));
    this.configDir = configDir ?? join(this.dir, 'config');
    this.http = nextPort++;
    this.wt = nextPort++;
    this.base = `http://localhost:${this.http}`;
  }

  async start() {
    this.proc = spawn(
      BINARY,
      [
        '--test',
        '--http-port', String(this.http),
        '--wt-port', String(this.wt),
        '--log-dir', join(this.dir, 'logs'),
        '--downloads-dir', join(this.dir, 'downloads'),
        '--fps', '30',
      ],
      { env: { ...process.env, GLASS_CONFIG_DIR: this.configDir, GLASS_TEST_OS: this.os }, stdio: 'ignore' },
    );
    for (let i = 0; i < 100; i++) {
      try {
        const r = await fetch(`${this.base}/config.json`);
        if (r.ok) return this;
      } catch {}
      await new Promise((r) => setTimeout(r, 100));
    }
    throw new Error(`host on :${this.http} didn't start; see ${this.dir}/logs/host.log`);
  }

  async stop() {
    if (!this.proc) return;
    const exited = new Promise((r) => this.proc.once('exit', r));
    this.proc.kill('SIGTERM');
    await exited;
    this.proc = undefined;
  }

  get token() {
    return readFileSync(join(this.configDir, 'token'), 'utf8').trim();
  }

  url({ token = true } = {}) {
    return token ? `${this.base}/#t=${this.token}` : `${this.base}/`;
  }

  async get(path) {
    const r = await fetch(`${this.base}${path}`);
    const type = r.headers.get('content-type') ?? '';
    return type.includes('json') ? r.json() : r.text();
  }

  async post(path, body = '') {
    const r = await fetch(`${this.base}${path}`, { method: 'POST', body });
    expect(r.ok, `POST ${path}`).toBeTruthy();
  }

  async clearInput() {
    await fetch(`${this.base}/test/input`, { method: 'DELETE' });
  }

  /** Recorded input events, optionally filtered by type. */
  async input(type) {
    const { events, pastes } = await this.get('/test/input');
    return { events: type ? events.filter((e) => e.t === type) : events, pastes };
  }

  /** Client log lines (forwarded by the page) matching `msg`. */
  clientLog(msg) {
    const file = join(this.dir, 'logs', 'client.log');
    if (!existsSync(file)) return [];
    return readFileSync(file, 'utf8')
      .split('\n')
      .filter(Boolean)
      .map((l) => JSON.parse(l.slice(l.indexOf(' ') + 1)))
      .filter((e) => (msg instanceof RegExp ? msg.test(e.msg) : e.msg === msg));
  }
}

/** Waits until the HUD shows `count` decoded streams (each "640×360"). */
export async function expectVideo(page, count = 1) {
  await expect
    .poll(async () => ((await hudText(page)).split('\n')[0].match(/640×360/g) ?? []).length, { timeout: 15_000 })
    .toBe(count);
}

export async function hudText(page) {
  return (await page.locator('#hud').textContent()) ?? '';
}

/** Parses the HUD's stats lines. */
export async function stats(page) {
  const hud = await hudText(page);
  const num = (re) => Number(hud.match(re)?.[1] ?? NaN);
  return {
    latency: num(/latency ([\d.]+) ms/),
    fps: num(/(\d+) fps/),
    dropped: num(/dropped (\d+)/),
    audioPackets: num(/audio (\d+) pkt\/s/),
    connected: /connected/.test(hud.split('\n')[0]),
  };
}

/** Opens a toolbar menu and clicks the item whose label starts with `label`. */
export async function pickMenuItem(page, button, label) {
  await page.locator(`#${button}`).click();
  await page.locator('.panel:not(.hidden) button', { hasText: label }).first().click();
}

/** Dispatches pointer events like a finger would, at a fraction of the canvas. */
export async function touch(page, canvasIndex, points) {
  await page.evaluate(
    ({ canvasIndex, points }) => {
      const canvas = document.querySelectorAll('#screens canvas')[canvasIndex];
      const r = canvas.getBoundingClientRect();
      for (const { type, id = 1, fx, fy } of points) {
        canvas.dispatchEvent(
          new PointerEvent(type, {
            bubbles: true,
            cancelable: true,
            pointerType: 'touch',
            pointerId: id,
            clientX: r.left + r.width * fx,
            clientY: r.top + r.height * fy,
          }),
        );
      }
    },
    { canvasIndex, points },
  );
}
