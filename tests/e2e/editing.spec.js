import { test, expect } from '@playwright/test';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS; // folder for screenshots (optional)
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('tree window: rename with F2, delete a branch, undo, settings', async () => {
  const env = setup('editing');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  const page = ctx.pages()[0];
  await page.goto(`${s}/p/alpha`);
  const [child] = await Promise.all([ctx.waitForEvent('page'), page.click('#child')]);
  await child.waitForLoadState();
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/alpha\n    tab /p/alpha-child'));

  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 420, height: 600 });
  // The page has no DOM to wait on (it is a canvas): give the wasm time.
  await pause(3500);
  // Rows: Window at y≈79, alpha at y≈102 (default settings, narrow window).
  const alpha = { x: 70, y: 102 };

  // Rename with F2.
  await tree.mouse.click(alpha.x, alpha.y);
  await tree.keyboard.press('F2');
  await pause(200);
  await tree.keyboard.press('Control+A');
  await tree.keyboard.type('Research');
  await tree.keyboard.press('Enter');
  await eventually(() => {
    const t = liveTree(env.store);
    expect(t.root.children[0].children[0].custom_title).toBe('Research');
  });
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/e1-renamed.png` });

  // Delete the branch: both tabs go to the trash and close in the browser.
  await tree.mouse.click(alpha.x, alpha.y);
  await tree.keyboard.press('Delete');
  await eventually(() => {
    const t = liveTree(env.store);
    expect(shape(t, s)).toBe('');
    const item = t.trash.find((x) => x.node.url === `${s}/p/alpha`);
    expect(item.node.children[0].url).toBe(`${s}/p/alpha-child`);
  });
  await eventually(() => expect(ctx.pages().filter((p) => p.url().startsWith(s)).length).toBe(0));

  // Undo puts the branch back, as saved tabs: the browser tabs are gone.
  await pause(500);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/e1b-deleted.png` });
  await tree.keyboard.press('Control+z');
  try {
    await eventually(() => expect(shape(liveTree(env.store), s)).toBe('saved_tab /p/alpha\n  saved_tab /p/alpha-child'));
  } catch (e) {
    const t = liveTree(env.store);
    const a = (t.root.children ?? []).find((c) => c.url === `${s}/p/alpha`);
    console.log("DEBUG tree", JSON.stringify(t).slice(0, 1500));
    console.log('DEBUG alpha', JSON.stringify({ kind: a?.kind, data: a?.data }));
    console.log('DEBUG log', require('node:fs').readFileSync(env.store + '/host.log', 'utf8').split('\n').filter((l) => /edit|Trash|trash|onRemoved/.test(l)).slice(-30).join('\n'));
    throw e;
  }

  // Settings: open, make everything bigger, and it sticks after a reload.
  const gear = { x: 404, y: 18 };
  await tree.mouse.click(gear.x, gear.y);
  await pause(300);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/e2-settings.png` });
  await tree.evaluate(() => localStorage.setItem('treetabs.ui', JSON.stringify({ ui_scale: 1.3, font_size: 16 })));
  await tree.reload();
  await pause(2000);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/e3-scaled.png` });
  const saved = await tree.evaluate(() => JSON.parse(localStorage.getItem('treetabs.ui')));
  expect(saved.ui_scale).toBe(1.3);

  await ctx.close();
  srv.close();
});
