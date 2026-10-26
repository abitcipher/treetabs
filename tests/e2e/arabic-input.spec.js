import { test, expect } from '@playwright/test';
import http from 'node:http';
import { setup, launch, liveTree, eventually } from './helpers.js';

const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('typing Arabic in the search box finds the tab; nothing panics', async () => {
  const env = setup('arabic-input');
  const titles = ['مرحبا بالعالم', 'أخبار اليوم', 'Plain English'];
  const srv = http.createServer((req, res) => {
    res.setHeader('content-type', 'text/html; charset=utf-8');
    res.end(`<title>${titles[Number(req.url.slice(3))]}</title>`);
  });
  await new Promise((r) => srv.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${srv.address().port}`;
  const ctx = await launch(env.profile);
  await ctx.pages()[0].goto(`${base}/p/0`);
  for (let i = 1; i < titles.length; i++) await (await ctx.newPage()).goto(`${base}/p/${i}`);
  await eventually(() => expect(liveTree(env.store).root.children[0].children.length).toBe(3));
  const sw = ctx.serviceWorkers()[0];
  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  const errors = [];
  tree.on('pageerror', (e) => errors.push(String(e)));
  tree.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
  await tree.setViewportSize({ width: 900, height: 300 });
  await tree.evaluate(() => localStorage.setItem('treetabs.ui', JSON.stringify({ panel_open: false })));
  await tree.reload();
  await pause(3000);
  await tree.mouse.click(200, 17);
  await tree.keyboard.type('أخبار ال');
  await tree.keyboard.press('Home');
  await tree.keyboard.press('ArrowLeft');
  await tree.keyboard.press('End');
  await pause(500);
  // One result: the middle tab (unfiltered, this row would be the first
  // one). Double-click focuses it in the browser.
  await tree.mouse.dblclick(120, 80);
  await eventually(async () => {
    const tabs = await sw.evaluate(() => chrome.tabs.query({}));
    const focused = tabs.find((t) => t.active && t.url.includes('/p/'));
    expect(focused?.url).toBe(`${base}/p/1`);
  });
  expect(errors).toEqual([]);
  await ctx.close();
  srv.close();
});
