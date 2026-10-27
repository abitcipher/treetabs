// Store listing screenshots, taken in the default mode (the extension on
// its own, no companion app registered in the profile), from demo pages.
// Usage: cd tests/e2e && node store-screenshots.mjs <out dir> <width> <height> [probe]
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { launch } from './helpers.js';

const [OUT, W, H, PROBE] = [process.argv[2], Number(process.argv[3] ?? 1280), Number(process.argv[4] ?? 800), process.argv[5]];
fs.mkdirSync(OUT, { recursive: true });
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const PAGES = {
  rust: ['The Rust Programming Language', 18], egui: ['egui – an easy-to-use immediate mode GUI', 260],
  egui_docs: ['egui::Ui – Rust docs', 220], tree: ['egui_ltreeview: tree view widget', 300],
  news: ['Hacker News', 25], hn_item: ['Show HN: a fast tab tree manager', 30],
  weather: ['Weather — Kyiv, 14°', 200], maps: ['Maps — Lisbon old town', 120],
  recipe: ['Pastéis de nata recipe', 45], mail: ['Inbox (3) — Mail', 0],
  long: ['A very long article title about browser tab management, memory, and why tab trees help', 160],
};
const srv = http.createServer((req, res) => {
  const m = req.url.match(/^\/(icon\/)?([a-z_]+)/);
  const key = m?.[2] ?? 'rust';
  const [title, hue] = PAGES[key] ?? ['Page', 0];
  if (m?.[1]) {
    res.setHeader('content-type', 'image/svg+xml');
    return res.end(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16" rx="4" fill="hsl(${hue},70%,48%)"/><circle cx="8" cy="8" r="3.2" fill="white"/></svg>`);
  }
  res.setHeader('content-type', 'text/html; charset=utf-8');
  res.end(`<title>${title}</title><link rel="icon" href="/icon/${key}.svg">` + Object.keys(PAGES).map((k) => `<a id="${k}" href="/${k}" target="_blank">${k}</a>`).join(' '));
});
await new Promise((r) => srv.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${srv.address().port}`;

const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'treetabs-store-shots-'));
const ctx = await launch(profile, { viewport: null });
const sw = ctx.serviceWorkers()[0];
for (let i = 0; i < 100 && (await sw.evaluate(() => treetabsMode())) !== 'alone'; i++) await pause(100);

const p = ctx.pages()[0];
await p.goto(`${base}/rust`);
for (const k of ['egui', 'tree']) {
  const [c] = await Promise.all([ctx.waitForEvent('page'), p.click(`#${k}`)]);
  await c.waitForLoadState();
  if (k === 'egui') {
    const [d] = await Promise.all([ctx.waitForEvent('page'), c.click('#egui_docs')]);
    await d.waitForLoadState();
  }
}
const [, w2] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate((u) => chrome.windows.create({ url: u }), `${base}/news`)]);
for (const k of ['weather', 'mail']) {
  await Promise.all([ctx.waitForEvent('page'), sw.evaluate(([w, u]) => chrome.tabs.create({ windowId: w, url: u }), [w2.id, `${base}/${k}`])]);
}
await pause(800);
const gone = await ctx.newPage();
await gone.goto(`${base}/hn_item`);
await pause(500);
await gone.close();
await pause(800);

const [view] = await Promise.all([ctx.waitForEvent('page', (pg) => pg.url().includes('tree.html')), sw.evaluate(() => treetabsOpenTree())]);
await view.waitForLoadState();
// Saved windows, a group and a note, as if kept from earlier sessions.
await view.evaluate(async ([base, PAGES]) => {
  const port = chrome.runtime.connect({ name: 'viewer' });
  const snap = await new Promise((res) => port.onMessage.addListener((m) => { const x = JSON.parse(m); if (x.type === 'snapshot') res(x); }));
  let id = 900000;
  const saved = (k) => ({ id: id++, kind: 'saved_tab', title: PAGES[k][0], url: `${base}/${k}`, favicon: `${base}/icon/${k}.svg` });
  const nodes = [
    { id: id++, kind: 'saved_window', custom_title: 'Trip to Lisbon', children: [saved('maps'), saved('recipe')] },
    { id: id++, kind: 'group', custom_title: 'Reading list', children: [{ id: id++, kind: 'note', note: 'Read these before Friday' }, saved('long')] },
  ];
  const ops = nodes.map((node, index) => ({ op: 'insert', parent: snap.tree.root.id, index, node }));
  port.postMessage(JSON.stringify({ type: 'edit', req: 1, ops }));
  port.postMessage(JSON.stringify({ type: 'export_now' }));
  await new Promise((r) => setTimeout(r, 800));
  port.disconnect();
}, [base, PAGES]);

const ui = (o) => view.evaluate((o) => localStorage.setItem('treetabs.ui', JSON.stringify(o)), o);
const shot = async (name) => { await pause(700); await view.screenshot({ path: path.join(OUT, `${name}.png`) }); console.log(name); };
const reload = async (o) => { await ui(o); await view.reload(); await pause(3500); };
await view.setViewportSize({ width: W, height: H });

// The steps for each size: [name, ui settings, actions], actions as
// [kind, x, y | text] with positions for this size.
const STEPS = JSON.parse(fs.readFileSync(path.join(OUT, 'steps.json'), 'utf8'));
if (PROBE) {
  await reload({ panel_open: true, contrast: 0.5, theme: 'light' });
  await shot('probe');
} else {
  for (const [name, settings, actions] of STEPS) {
    await reload({ contrast: 0.5, theme: 'light', ...settings });
    for (const [kind, a, b] of actions) {
      if (kind === 'click') await view.mouse.click(a, b);
      if (kind === 'right') await view.mouse.click(a, b, { button: 'right' });
      if (kind === 'type') await view.keyboard.type(a);
      await pause(400);
    }
    await shot(name);
  }
}
await ctx.close();
srv.close();
