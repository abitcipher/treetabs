// Shared setup: build artifacts, a temp browser profile with the native host registered, a temp store, and a local page server.
import { chromium } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';

export const ROOT = path.resolve(import.meta.dirname, '../..');
export const BIN = path.join(ROOT, 'target/debug');
export const EXT = path.join(ROOT, 'extension/dist/chromium');

// Tests must never see the user's real config (it points at the real store).
const TEST_CONFIG = fs.mkdtempSync(path.join(os.tmpdir(), 'treetabs-e2e-config-'));
export const TEST_ENV = { ...process.env, XDG_CONFIG_HOME: TEST_CONFIG, RUST_LOG: process.env.RUST_LOG ?? 'info' };
delete TEST_ENV.TREETABS_DIR;

export function cli(...args) {
  return execFileSync(path.join(BIN, 'treetabs'), args, { encoding: 'utf8', env: TEST_ENV });
}

export function setup(name) {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), `treetabs-e2e-${name}-`));
  const profile = path.join(base, 'profile');
  const store = path.join(base, 'store');
  fs.mkdirSync(profile, { recursive: true });
  cli('install-host', '--user-data-dir', profile, '--store', store, '--host', path.join(BIN, 'treetabs-host'));
  return { base, profile, store };
}

export async function launch(profile, options = {}) {
  const ctx = await chromium.launchPersistentContext(profile, {
    ...options,
    channel: 'chromium',
    headless: !process.env.HEADED,
    env: TEST_ENV,
    args: [`--disable-extensions-except=${EXT}`, `--load-extension=${EXT}`],
  });
  if (ctx.serviceWorkers().length === 0) await ctx.waitForEvent('serviceworker');
  // The worker's wasm starts asynchronously: wait until it is up.
  const sw = ctx.serviceWorkers()[0];
  for (let i = 0; i < 100 && !(await sw.evaluate(() => typeof treetabsOpenTree === 'function')); i++) {
    await new Promise((r) => setTimeout(r, 50));
  }
  return ctx;
}

export function liveTree(store) {
  return JSON.parse(cli('live', '--store', store, '--json'));
}

/// Outline like the Rust tests: "kind path" per node, two spaces per level.
/// Only nodes with our test pages (or windows/groups containing them) are shown.
export function shape(tree, server) {
  const lines = [];
  const walk = (n, d) => {
    for (const c of n.children ?? []) {
      const url = c.url ?? '';
      const label = url.startsWith(server) ? url.slice(server.length) : null;
      const sub = [];
      const before = lines.length;
      if (label !== null || /window|group/.test(c.kind)) {
        lines.push(`${'  '.repeat(d)}${c.kind}${label !== null ? ' ' + label : ''}`);
      }
      walk(c, d + 1);
      // Drop windows that hold none of our pages.
      if (label === null && lines.length === before + 1) lines.pop();
    }
  };
  walk(tree.root, 0);
  return lines.join('\n');
}

export async function eventually(fn, { timeout = 15000, interval = 250 } = {}) {
  const start = Date.now();
  let last;
  for (;;) {
    try {
      return await fn();
    } catch (e) {
      last = e;
    }
    if (Date.now() - start > timeout) throw last;
    await new Promise((r) => setTimeout(r, interval));
  }
}

export async function pageServer() {
  const srv = http.createServer((req, res) => {
    if (req.url.startsWith('/icon/')) {
      // A colored square per page, so favicons are easy to see.
      const hue = [...req.url].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 0);
      res.setHeader('content-type', 'image/svg+xml');
      return res.end(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16" rx="3" fill="hsl(${hue},70%,50%)"/></svg>`);
    }
    const name = req.url.replace(/^\/p\//, '');
    res.setHeader('content-type', 'text/html');
    res.end(`<title>${name}</title><link rel="icon" href="/icon/${name}.svg"><a id="child" href="/p/${name}-child" target="_blank">child</a>`);
  });
  await new Promise((r) => srv.listen(0, '127.0.0.1', r));
  const url = `http://127.0.0.1:${srv.address().port}`;
  return { url, close: () => srv.close() };
}
