import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('trash: keep-for option is saved with the store; empty trash asks first', async () => {
  const env = setup('trash');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  await ctx.pages()[0].goto(`${s}/p/keep`);
  const gone = await ctx.newPage();
  await gone.goto(`${s}/p/gone`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/keep\n  tab /p/gone'));
  await gone.close();
  await eventually(() => expect(liveTree(env.store).trash.length).toBe(1));

  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 1100, height: 500 });
  await pause(3500);
  await tree.mouse.click(1020, 18); // Trash tab
  await pause(500);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/t1-trash.png` });
  if (process.env.LOOK) return;

  // "Remove items older than": pick 30 days.
  await tree.mouse.click(1000, 116);
  await pause(300);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/t2-combo.png` });
  await tree.mouse.click(1000, 180);
  await eventually(() => expect(JSON.parse(fs.readFileSync(path.join(env.store, 'settings.json'), 'utf8')).trash_days).toBe(30));
  // Recent items stay.
  expect(liveTree(env.store).trash.length).toBe(1);

  // Empty trash: asks, then removes.
  await tree.mouse.click(837, 137);
  await pause(400);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/t3-confirm.png` });
  expect(liveTree(env.store).trash.length).toBe(1);
  await tree.mouse.click(428, 287); // "Empty trash" in the dialog
  await eventually(() => expect(liveTree(env.store).trash ?? []).toEqual([]));
  // ...after a copy that still has it.
  const [copy] = fs.readdirSync(path.join(env.store, 'exports'));
  const saved = JSON.parse(fs.readFileSync(path.join(env.store, 'exports', copy), 'utf8'));
  expect(saved.trash.map((t) => t.node.url)).toEqual([`${s}/p/gone`]);
  await ctx.close();
  srv.close();
});
