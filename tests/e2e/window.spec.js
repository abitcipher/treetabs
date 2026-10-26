import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

test('toolbar action opens the tree window, which shows the live tree', async () => {
  const env = setup('window');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  await page.goto(`${s}/p/a`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/a'));

  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 420, height: 700 });
  const errors = [];
  tree.on('pageerror', (e) => errors.push(String(e)));
  tree.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
  expect(tree.url()).toMatch(/tree\.html$/);
  // The page connected as a viewer through the extension and the host.
  await eventually(() => expect(fs.readFileSync(path.join(env.store, 'host.log'), 'utf8')).toMatch(/tree window \(Viewer\)/));
  // Its own window is not part of the tree.
  await new Promise((r) => setTimeout(r, 1500));
  expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/a');
  // A second click focuses the same window instead of opening another.
  await sw.evaluate(() => treetabsOpenTree());
  await new Promise((r) => setTimeout(r, 500));
  expect(ctx.pages().filter((p) => p.url().endsWith('tree.html')).length).toBe(1);
  if (process.env.SHOT) await tree.screenshot({ path: process.env.SHOT });
  // Keyboard shortcut for the same action.
  const cmds = await sw.evaluate(() => chrome.commands.getAll());
  expect(cmds.find((c) => c.name === '_execute_action').shortcut).toBe('Alt+T');
  expect(errors).toEqual([]);
  await ctx.close();
  srv.close();
});

test('the tree window opens where it was left, at the same size', async () => {
  const env = setup('bounds');
  // viewport: null, or Playwright pins every window to its own size.
  const ctx = await launch(env.profile, { viewport: null });
  const sw = ctx.serviceWorkers()[0];
  const pause = (ms) => new Promise((r) => setTimeout(r, ms));
  const treeWindow = () =>
    sw.evaluate(async () => {
      const [tab] = await chrome.tabs.query({ url: chrome.runtime.getURL('tree.html') });
      return tab ? chrome.windows.get(tab.windowId) : null;
    });
  // First time: docked at the left, 420 wide.
  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await pause(1500);
  const first = await treeWindow();
  expect(first.width).toBe(420);
  // Move and resize it, close it, open it again.
  await sw.evaluate((id) => chrome.windows.update(id, { left: 150, top: 60, width: 560, height: 480 }), first.id);
  await pause(2500);
  const moved = await treeWindow();
  await tree.close();
  await pause(500);
  await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await pause(1500);
  const again = await treeWindow();
  expect(again.id).not.toBe(first.id);
  for (const k of ['left', 'top', 'width', 'height']) expect(Math.abs(again[k] - moved[k])).toBeLessThanOrEqual(2);
  expect(again.width).toBe(560);
  await ctx.close();
});
