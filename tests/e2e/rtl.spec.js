import { test, expect } from '@playwright/test';
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('arabic (right to left): settings and panel tabs work, nothing panics', async () => {
  const env = setup('rtl');
  const srv = await pageServer();
  const ctx = await launch(env.profile);
  await ctx.pages()[0].goto(`${srv.url}/p/one`);
  const gone = await ctx.newPage();
  await gone.goto(`${srv.url}/p/gone`);
  await gone.close();
  await eventually(() => expect(liveTree(env.store).trash.length).toBe(1));
  const sw = ctx.serviceWorkers()[0];
  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  const errors = [];
  tree.on('pageerror', (e) => errors.push(String(e)));
  tree.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
  await tree.setViewportSize({ width: 1000, height: 480 });
  await tree.evaluate(() => localStorage.setItem('treetabs.ui', JSON.stringify({ language: 'ar-eg' })));
  await tree.reload();
  await pause(3000);
  // Mirrored: settings at the far left, then the panel tabs.
  await tree.mouse.click(13, 18);
  await pause(600);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/r1-settings.png` });
  await tree.keyboard.press('Escape');
  await pause(300);
  await tree.mouse.click(80, 18); // Trash tab
  await pause(600);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/r2-trash.png` });
  const ui = await tree.evaluate(() => JSON.parse(localStorage.getItem('treetabs.ui')));
  expect(ui.panel_tab).toBe('trash');
  expect(errors).toEqual([]);
  await ctx.close();
  srv.close();
});
