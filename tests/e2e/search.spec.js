import { test, expect } from '@playwright/test';
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('search: duplicates filter closes the extra copies, keeping one', async () => {
  const env = setup('search');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  await ctx.pages()[0].goto(`${s}/p/docs`);
  const dup = await ctx.newPage();
  await dup.goto(`${s}/p/docs#part`); // same page: the fragment doesn't count
  const other = await ctx.newPage();
  await other.goto(`${s}/p/news`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/docs\n  tab /p/docs#part\n  tab /p/news'));

  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 1100, height: 500 });
  await pause(3500);
  await tree.mouse.click(375, 17); // filter menu
  await pause(300);
  await tree.mouse.click(410, 125); // Duplicates
  await pause(500);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/s1-duplicates.png` });
  await tree.mouse.click(367, 57); // Close 1 duplicate(s)
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/docs\n  tab /p/news'));
  await eventually(() => expect(ctx.pages().filter((p) => p.url().startsWith(`${s}/p/docs`)).length).toBe(1));
  await ctx.close();
  srv.close();
});
