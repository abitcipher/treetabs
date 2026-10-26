// Screenshots for docs/GUIDE.md, from demo data in a throwaway store.
// Usage: cd tests/e2e && node docs-screenshots.mjs   (after cargo xtask build-ext)
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { setup, launch, cli, ROOT, liveTree, eventually } from './helpers.js';

const OUT = path.join(ROOT, 'docs/images');
fs.mkdirSync(OUT, { recursive: true });
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

// Demo pages: title and icon color.
const PAGES = {
  rust: ['The Rust Programming Language', 18],
  egui: ['egui – an easy-to-use immediate mode GUI', 260],
  egui_docs: ['egui::Ui – Rust docs', 220],
  tree: ['egui_ltreeview: tree view widget', 300],
  news: ['Hacker News', 25],
  hn_item: ['Show HN: a fast tab tree manager', 30],
  weather: ['Weather — Kyiv, 14°', 200],
  maps: ['Maps — Lisbon old town', 120],
  recipe: ['Pastéis de nata recipe', 45],
  mail: ['Inbox (3) — Mail', 0],
  dup: ['Hacker News', 25],
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
  const links = Object.keys(PAGES).map((k) => `<a id="${k}" href="/${k}" target="_blank">${k}</a>`).join(' ');
  res.end(`<title>${title}</title><link rel="icon" href="/icon/${key}.svg">${links}`);
});
await new Promise((r) => srv.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${srv.address().port}`;

// Saved windows, a group and a note, as if kept from earlier sessions.
const env = setup('docs');
let id = 100;
const saved = (key, children = []) => ({ id: id++, kind: 'saved_tab', title: PAGES[key][0], url: `${base}/${key}`, favicon: `${base}/icon/${key}.svg`, children });
const tree = {
  format: 'treetabs', version: 1,
  root: { id: 1, kind: 'session', children: [
    { id: id++, kind: 'saved_window', custom_title: 'Trip to Lisbon', children: [saved('maps'), saved('recipe')] },
    { id: id++, kind: 'group', custom_title: 'Reading list', children: [
      { id: id++, kind: 'note', note: 'Read these before Friday' },
      saved('long'), saved('hn_item'),
    ] },
  ] },
};
const demo = path.join(env.base, 'demo.json');
fs.writeFileSync(demo, JSON.stringify(tree));
cli('import', demo, '--store', env.store);

const ctx = await launch(env.profile, { viewport: null });
const sw = ctx.serviceWorkers()[0];
// Window 1: Rust work, with tabs opened from tabs (nested).
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
// Window 2: everyday.
const [, w2] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate((u) => chrome.windows.create({ url: u }), `${base}/news`)]);
for (const k of ['weather', 'mail', 'news']) {
  await Promise.all([ctx.waitForEvent('page'), sw.evaluate(([w, u]) => chrome.tabs.create({ windowId: w, url: u }), [w2.id, `${base}/${k}`])]);
}
await pause(1000);
// One closed tab in the trash.
const gone = await ctx.newPage();
await gone.goto(`${base}/hn_item`);
await pause(500);
await gone.close();
await eventually(() => expect(liveTree(env.store).trash?.length ?? 0).toBeGreaterThan(0));

const [view] = await Promise.all([ctx.waitForEvent('page', (pg) => pg.url().includes('tree.html')), sw.evaluate(() => treetabsOpenTree())]);
const ui = (o) => view.evaluate((o) => localStorage.setItem('treetabs.ui', JSON.stringify(o)), o);
const shot = async (name, clip) => {
  await pause(700);
  await view.screenshot({ path: path.join(OUT, `${name}.png`), clip });
  console.log(name);
};
const reload = async (o) => {
  await ui(o);
  await view.reload();
  await pause(3500);
};

// 1. Wide: tree and details.
await view.setViewportSize({ width: 1100, height: 560 });
await reload({ panel_open: true, contrast: 0.5 });
await view.mouse.click(130, 128); // select "egui"
await shot('overview');
// 2. Narrow, as the toolbar button opens it.
await view.setViewportSize({ width: 420, height: 640 });
await reload({ panel_open: false, contrast: 0.5 });
await shot('tree-window');
// 3. Hover buttons and context menu.
await view.setViewportSize({ width: 760, height: 520 });
await reload({ panel_open: false, contrast: 0.5 });
await view.mouse.move(160, 244); // "The Rust Programming Language", open, with children
await shot('row-buttons', { x: 0, y: 200, width: 760, height: 130 });
await view.mouse.click(160, 244, { button: 'right' });
await shot('context-menu');
await view.keyboard.press('Escape');
// 4. Search with highlight, and the duplicates filter.
await reload({ panel_open: false, contrast: 0.5 });
await view.mouse.click(200, 17);
await view.keyboard.type('egui');
await shot('search', { x: 0, y: 0, width: 760, height: 200 });
await view.keyboard.press('Escape');
await view.mouse.click(375, 17);
await pause(300);
await shot('filter-menu', { x: 0, y: 0, width: 760, height: 260 });
await view.mouse.click(410, 125);
await shot('duplicates', { x: 0, y: 0, width: 760, height: 200 });
// 5. Panel tabs: trash and backups.
await view.setViewportSize({ width: 1100, height: 520 });
await reload({ panel_open: true, panel_tab: 'trash', contrast: 0.5 });
await shot('trash');
await reload({ panel_open: true, panel_tab: 'backups', contrast: 0.5 });
await view.mouse.click(852, 334); // Save a copy now
await shot('data');
// 6. Settings: its three tabs.
await reload({ panel_open: false, contrast: 0.5 });
await view.mouse.click(1085, 18);
await shot('settings');
await view.mouse.click(849, 82); // Storage
await shot('settings-storage', { x: 680, y: 30, width: 420, height: 260 });
await view.mouse.click(905, 82); // About
await shot('settings-about', { x: 680, y: 30, width: 420, height: 330 });
// 7. Dark theme, and languages.
await reload({ panel_open: true, theme: 'dark', contrast: 0.5 });
await view.mouse.click(130, 128);
await shot('dark');
await reload({ panel_open: true, language: 'uk', contrast: 0.5 });
await shot('language-uk');
await reload({ panel_open: true, language: 'zh', contrast: 0.5 });
await shot('language-zh');
await reload({ panel_open: true, language: 'ar', contrast: 0.5 });
await shot('language-ar');
// Store listing images: 1280x800 (Chrome Web Store; also fine for the
// Firefox and Opera listings).
fs.mkdirSync(path.join(OUT, 'store'), { recursive: true });
await view.setViewportSize({ width: 1280, height: 800 });
await reload({ panel_open: true, contrast: 0.5 });
await view.mouse.click(130, 128);
await shot('store/1-tree');
await view.mouse.click(160, 244, { button: 'right' });
await shot('store/2-menu');
await view.keyboard.press('Escape');
await reload({ panel_open: true, panel_tab: 'backups', contrast: 0.5 });
await view.mouse.click(240, 17);
await view.keyboard.type('egui');
await shot('store/3-search-backups');
// 8. The bookmark mirror, in the browser's bookmark manager.
const bm = await ctx.newPage();
await bm.setViewportSize({ width: 1000, height: 560 });
const [folder] = await sw.evaluate(() => chrome.bookmarks.search({ title: 'treetabs' }));
const [win] = await sw.evaluate((id) => chrome.bookmarks.getChildren(id), folder.id);
await bm.goto(`chrome://bookmarks/?id=${folder.id}`).catch(() => {});
await pause(1500);
await bm.screenshot({ path: path.join(OUT, 'bookmarks.png') });
console.log('bookmarks');

await ctx.close();
srv.close();

function expect(v) {
  return { toBeGreaterThan: (n) => { if (!(v > n)) throw new Error(`${v} <= ${n}`); } };
}
