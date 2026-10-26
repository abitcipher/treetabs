import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';
import { setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

const SHOTS = process.env.SHOTS;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

async function openTree(ctx, env, srv) {
  await ctx.pages()[0].goto(`${srv.url}/p/one`);
  await eventually(() => expect(shape(liveTree(env.store), srv.url)).toBe('window\n  tab /p/one'));
  const sw = ctx.serviceWorkers()[0];
  const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
  await tree.setViewportSize({ width: 1100, height: 600 });
  await pause(3500);
  return tree;
}

test('data tab: export in each format, import a file as a group', async () => {
  const env = setup('data');
  const srv = await pageServer();
  const ctx = await launch(env.profile, { acceptDownloads: true });
  const tree = await openTree(ctx, env, srv);
  await tree.mouse.click(945, 18); // Data tab
  await pause(500);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/data1.png` });
  if (process.env.LOOK) return;
  // treetabs JSON (the default format).
  const [dl] = await Promise.all([tree.waitForEvent('download'), tree.mouse.click(EXPORT.x, EXPORT.y)]);
  expect(dl.suggestedFilename()).toMatch(/^treetabs-\d{4}-\d\d-\d\d\.json$/);
  const json = JSON.parse(fs.readFileSync(await dl.path(), 'utf8'));
  expect(json.format).toBe('treetabs');
  expect(JSON.stringify(json)).toContain(`${srv.url}/p/one`);
  // Bookmarks HTML. (Chrome asks before a page's second download; a fresh
  // page doesn't need to.)
  await tree.reload(); // the Data tab stays open
  await pause(3000);
  await tree.mouse.click(FORMAT.x, FORMAT.y);
  await pause(300);
  await tree.mouse.click(FORMAT.x, FORMAT.htmlY);
  await pause(300);
  const [dl2] = await Promise.all([tree.waitForEvent('download'), tree.mouse.click(EXPORT.x, EXPORT.y)]);
  expect(dl2.suggestedFilename()).toMatch(/bookmarks\.html$/);
  expect(fs.readFileSync(await dl2.path(), 'utf8')).toContain(`<A HREF="${srv.url}/p/one">one</A>`);
  await pause(800);
  // Import the JSON back: added at the end as a group of saved tabs.
  const file = path.join(env.base, 'mine.json');
  fs.writeFileSync(file, JSON.stringify(json));
  const [chooser] = await Promise.all([tree.waitForEvent('filechooser'), tree.mouse.click(IMPORT.x, IMPORT.y)]);
  await chooser.setFiles(file);
  await eventually(() => expect(shape(liveTree(env.store), srv.url)).toBe('window\n  tab /p/one\ngroup\n  saved_window\n    saved_tab /p/one'));
  expect(liveTree(env.store).root.children[1].custom_title).toBe('Imported from mine.json');
  await ctx.close();
  srv.close();
});

test('settings: shows where the tree is, and moves it to another folder', async () => {
  const env = setup('movestore');
  const srv = await pageServer();
  const ctx = await launch(env.profile);
  const tree = await openTree(ctx, env, srv);
  await tree.mouse.click(1085, 18); // settings
  await pause(500);
  await tree.mouse.click(STORAGE_TAB.x, STORAGE_TAB.y);
  await pause(400);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/storage1.png` });
  if (process.env.LOOK) return;
  const to = path.join(env.base, 'moved-store');
  // Replace the folder in the field and move.
  await tree.mouse.click(MOVE_FIELD.x, MOVE_FIELD.y);
  await tree.keyboard.press('Control+a');
  await tree.keyboard.type(to);
  await tree.mouse.click(MOVE_BUTTON.x, MOVE_BUTTON.y);
  await pause(400);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/storage2.png` });
  await tree.mouse.click(CONFIRM.x, CONFIRM.y);
  // The tree is in the new folder, the browser's host runs there again.
  await eventually(() => expect(fs.existsSync(path.join(to, 'tree.json'))).toBe(true), { timeout: 15000 });
  await eventually(() => expect(fs.existsSync(path.join(to, 'host.log'))).toBe(true), { timeout: 20000 });
  await eventually(() => expect(shape(liveTree(to), srv.url)).toBe('window\n  tab /p/one'), { timeout: 20000 });
  // The old folder is still there.
  expect(fs.existsSync(path.join(env.store, 'tree.json'))).toBe(true);
  await pause(1000);
  if (SHOTS) await tree.screenshot({ path: `${SHOTS}/storage3.png` });
  await ctx.close();
  srv.close();
});

const EXPORT = { x: 820, y: 121 };
const FORMAT = { x: 880, y: 100, htmlY: 166 };
const IMPORT = { x: 832, y: 218 };
const MOVE_FIELD = { x: 870, y: 177 };
const MOVE_BUTTON = { x: 1040, y: 177 };
const CONFIRM = { x: 408, y: 361 };
const STORAGE_TAB = { x: 849, y: 82 };
