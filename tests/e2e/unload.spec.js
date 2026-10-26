import { test, expect } from '@playwright/test';
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('unload: a tab is discarded in the browser and marked in the tree', async () => {
  const env = setup('unload');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  await ctx.pages()[0].goto(`${s}/p/a`);
  const b = await ctx.newPage();
  await b.goto(`${s}/p/b`);
  await ctx.pages()[0].bringToFront(); // b is not the active tab
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/a\n  tab /p/b'));

  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 1100, height: 500 });
  await pause(3500);
  await tree.mouse.click(70, 105, { button: 'right' }); // row of /p/b
  await pause(400);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/u1-menu.png` });
  if (process.env.LOOK) return;
  await tree.mouse.click(UNLOAD.x, UNLOAD.y);
  await eventually(() => {
    const tab = liveTree(env.store).root.children[0].children.find((t) => t.url === `${s}/p/b`);
    expect(tab.kind).toBe('tab');
    expect(tab.data.discarded).toBe(true);
  });
  // The flag came from Chrome's own tabs.onUpdated, so the browser did
  // discard it. (Playwright drops its connection to a discarded tab and
  // then to the browser, so nothing more can be asked here.)
  await ctx.close().catch(() => {});
  srv.close();
});
const UNLOAD = { x: 140, y: 193 };
