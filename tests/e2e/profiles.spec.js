import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { setup, launch, cli, BIN, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('two browser profiles in one tree: labels and a profile filter', async () => {
  const env = setup('profiles');
  // A second profile using the same store.
  const profile2 = path.join(env.base, 'profile2');
  fs.mkdirSync(profile2);
  cli('install-host', '--user-data-dir', profile2, '--store', env.store, '--host', path.join(BIN, 'treetabs-host'));
  const srv = await pageServer();
  const s = srv.url;
  const one = await launch(env.profile);
  await one.pages()[0].goto(`${s}/p/one`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/one'));
  const two = await launch(profile2);
  await two.pages()[0].goto(`${s}/p/two`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/one\nwindow\n  tab /p/two'));
  const names = JSON.parse(fs.readFileSync(path.join(env.store, 'profiles.json'), 'utf8'));
  expect(Object.values(names)).toEqual(['Chromium', 'Chromium']);

  const sw = one.serviceWorkers()[0];
  const [tree] = await Promise.all([one.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 1100, height: 500 });
  await pause(3500);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/p1-two-profiles.png` });
  const rows = { x: 0, y: 48, width: 400, height: 92 }; // both windows' rows
  const before = await tree.screenshot({ clip: rows });
  if (process.env.LOOK) return;
  // Filter menu > the second profile in the list.
  await tree.mouse.click(375, 17);
  await pause(300);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/p2-menu.png` });
  await tree.mouse.click(PROFILE_ITEM.x, PROFILE_ITEM.y);
  await pause(400);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/p3-filtered.png` });
  // One window is hidden now; clearing the filters brings it back.
  expect((await tree.screenshot({ clip: rows })).equals(before)).toBe(false);
  await tree.mouse.click(407, 17); // ✖
  await pause(400);
  expect((await tree.screenshot({ clip: rows })).equals(before)).toBe(true);
  await one.close();
  await two.close();
  srv.close();
});
const PROFILE_ITEM = { x: 430, y: 195 };
