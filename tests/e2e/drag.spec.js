import { test, expect } from '@playwright/test';
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('dragging a tab to another window in the tree moves it in the browser', async () => {
  const env = setup('drag');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  await ctx.pages()[0].goto(`${s}/p/a1`);
  const a2 = await ctx.newPage();
  await a2.goto(`${s}/p/a2`);
  await Promise.all([ctx.waitForEvent('page'), sw.evaluate((u) => chrome.windows.create({ url: u }), `${s}/p/b1`)]);
  const both = 'window\n  tab /p/a1\n  tab /p/a2\nwindow\n  tab /p/b1';
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe(both));

  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 1100, height: 500 });
  await pause(3500);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/g1-before.png` });
  // Drag a2 onto the second window's row.
  await tree.mouse.move(70, 105);
  await tree.mouse.down();
  await tree.mouse.move(75, 115, { steps: 4 });
  await tree.mouse.move(75, 128, { steps: 6 });
  await pause(200);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/g2-dragging.png` });
  await tree.mouse.up();
  await eventually(() => expect(shape(liveTree(env.store), s)).toMatch(/^window\n  tab \/p\/a1\nwindow\n(  tab \/p\/b1\n  tab \/p\/a2|  tab \/p\/a2\n  tab \/p\/b1)$/));
  const after = shape(liveTree(env.store), s);
  // The browser has it in that window too, in the same order.
  await eventually(async () => {
    const wins = await sw.evaluate(async () => (await chrome.windows.getAll({ populate: true })).map((w) => w.tabs.map((t) => t.url)));
    const second = wins.find((w) => w.some((u) => u.endsWith('/p/b1')));
    const expected = after.endsWith('a2') ? [`${s}/p/b1`, `${s}/p/a2`] : [`${s}/p/a2`, `${s}/p/b1`];
    expect(second).toEqual(expected);
  });
  // ...and the tree stays as the user put it.
  await pause(1000);
  expect(shape(liveTree(env.store), s)).toBe(after);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/g3-after.png` });
  await ctx.close();
  srv.close();
});
