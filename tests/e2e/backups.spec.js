import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('backups: save a copy now, add it back as a group of saved tabs, undo', async () => {
  const env = setup('backups');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  await ctx.pages()[0].goto(`${s}/p/a`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/a'));

  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 1100, height: 600 });
  await pause(3500);
  await tree.mouse.click(945, 18); // Backups tab
  await pause(500);
  await tree.mouse.click(852, 334); // Save a copy now
  const exports = path.join(env.store, 'exports');
  await eventually(() => expect(fs.readdirSync(exports).filter((f) => /^treetabs-\d{4}-\d\d-\d\d\.json$/.test(f)).length).toBe(1));
  await pause(500);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/b1-backups.png` });
  await tree.mouse.click(828, 398); // Add to tree
  await eventually(() =>
    expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/a\ngroup\n  saved_window\n    saved_tab /p/a'),
  );
  const group = liveTree(env.store).root.children[1];
  expect(group.custom_title).toMatch(/^Restored from treetabs-/);
  // The copy is not bound to the live tab.
  expect(group.children[0].children[0].data?.id).toBeUndefined();
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/b2-restored.png` });
  // Undo takes it out again.
  await tree.mouse.click(400, 400);
  await tree.keyboard.press('Control+z');
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/a'));
  await ctx.close();
  srv.close();
});
