import { test, expect } from '@playwright/test';
import { setup, launch, eventually, liveTree, shape, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const settings = (page) => page.evaluate(() => JSON.parse(localStorage.getItem('treetabs.ui') ?? '{}'));

test('side panel: width kept, tabs, stays closed; settings wheel and scale', async () => {
  const env = setup('details');
  const srv = await pageServer();
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  const long = 'x'.repeat(400);
  await ctx.pages()[0].goto(`${srv.url}/p/${long}`);
  await eventually(() => expect(shape(liveTree(env.store), srv.url)).toContain(long));

  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 1100, height: 600 });
  await tree.evaluate(() => localStorage.setItem('treetabs.ui', JSON.stringify({ details_width: 300 })));
  await tree.reload();
  await pause(3500);
  // Select the tab with the very long URL and title.
  await tree.mouse.click(70, 82);
  await pause(800);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/d1-long.png` });
  // The panel did not widen to fit the content.
  expect(Math.round((await settings(tree)).details_width ?? 300)).toBe(300);

  // Drag the panel edge (x = 1100 - 300) to the left by 150 px: saved.
  await tree.mouse.move(800, 300);
  await tree.mouse.down();
  await tree.mouse.move(700, 300, { steps: 5 });
  await tree.mouse.move(650, 300, { steps: 5 });
  await tree.mouse.up();
  await pause(500);
  const w = (await settings(tree)).details_width;
  expect(w).toBeGreaterThan(420);
  expect(w).toBeLessThan(480);

  // Toolbar tabs: Details | Trash. One panel at a time; clicking the active tab hides the panel.
  const trashTab = { x: 1020, y: 18 };
  const detailsTab = { x: 870, y: 18 };
  await tree.mouse.click(trashTab.x, trashTab.y);
  await pause(300);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/d2-trash.png` });
  expect((await settings(tree)).panel_tab).toBe('trash');
  await tree.mouse.click(detailsTab.x, detailsTab.y);
  await pause(300);
  expect((await settings(tree)).panel_tab).toBe('details');
  await tree.mouse.click(detailsTab.x, detailsTab.y);
  await pause(300);
  expect((await settings(tree)).panel_open).toBe(false);

  // Closed, it stays closed after a reload, and selecting a node does not open it.
  await tree.reload();
  await pause(3500);
  await tree.mouse.click(70, 82);
  await pause(500);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/d3-closed.png` });
  expect((await settings(tree)).panel_open).toBe(false);
  // Reopened, it comes back at the saved width.
  await tree.mouse.click(detailsTab.x, detailsTab.y);
  await pause(500);
  expect((await settings(tree)).panel_open).toBe(true);
  expect(Math.round((await settings(tree)).details_width)).toBe(Math.round(w));
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/d4-reopened.png` });

  // Settings: the mouse wheel moves sliders; the interface scale waits until the slider is let go, so nothing moves under the pointer.
  await tree.mouse.click(1085, 18);
  await pause(400);
  await tree.mouse.move(908, 161); // text size slider
  for (let i = 0; i < 3; i++) {
    await tree.mouse.wheel(0, -100);
    await pause(150);
  }
  await pause(300);
  expect((await settings(tree)).font_size).toBe(15.5);
  await tree.mouse.move(889, 135); // interface scale handle at 100%
  await tree.mouse.down();
  await tree.mouse.move(915, 135, { steps: 5 });
  await pause(1000);
  expect((await settings(tree)).ui_scale ?? 1).toBe(1);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/d5-dragging.png` });
  await tree.mouse.up();
  await pause(500);
  expect((await settings(tree)).ui_scale).toBeGreaterThan(1.2);
  await ctx.close();
  srv.close();
});
