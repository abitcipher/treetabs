// Without the companion app: the extension keeps the tree itself, in the
// browser's storage, and hands it over once the app is installed.
import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { BIN, cli, setup, launch, liveTree, shape, eventually, pageServer } from './helpers.js';

/// A browser profile with no native host registered.
function bareProfile(name) {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), `treetabs-e2e-${name}-`));
  const profile = path.join(base, 'profile');
  fs.mkdirSync(profile, { recursive: true });
  return { base, profile, store: path.join(base, 'store') };
}

async function mode(sw) {
  return sw.evaluate(() => treetabsMode());
}

/// The tree the extension keeps (only while it runs without the app).
async function ownTree(sw) {
  const json = await sw.evaluate(() => treetabsTree());
  return json ? JSON.parse(json) : null;
}

/// Keys the extension keeps for its own tree.
async function storedKeys(sw) {
  return sw.evaluate(async () => Object.keys(await chrome.storage.local.get(null)).filter((k) => k.startsWith('tt.')));
}

async function bookmarked(sw, url) {
  return sw.evaluate(async (url) => (await chrome.bookmarks.search({ url })).length, url);
}

test('without the companion app the tree is kept in the browser', async () => {
  const env = bareProfile('alone');
  const srv = await pageServer();
  const s = srv.url;
  let ctx = await launch(env.profile);
  let sw = ctx.serviceWorkers()[0];
  await eventually(async () => expect(await mode(sw)).toBe('alone'));

  const page = ctx.pages()[0] ?? (await ctx.newPage());
  await page.goto(`${s}/p/a`);
  const [child] = await Promise.all([ctx.waitForEvent('page'), page.click('#child')]);
  await child.waitForLoadState();
  await eventually(async () => {
    expect(shape(await ownTree(sw), s)).toBe('window\n  tab /p/a\n    tab /p/a-child');
  });
  // Mirrored to bookmarks as with the app.
  await eventually(async () => expect(await bookmarked(sw, `${s}/p/a-child`)).toBe(1));

  // The tree window shows it.
  await sw.evaluate(() => treetabsOpenTree());
  const tree = await eventually(async () => {
    const p = ctx.pages().find((p) => p.url().endsWith('/tree.html'));
    expect(p).toBeTruthy();
    return p;
  });
  await tree.waitForLoadState();

  // Give the storage writes a moment, then restart the browser.
  await new Promise((r) => setTimeout(r, 1000));
  expect((await storedKeys(sw)).includes('tt.tree')).toBe(true);
  await ctx.close();
  ctx = await launch(env.profile);
  sw = ctx.serviceWorkers()[0];
  await eventually(async () => expect(await mode(sw)).toBe('alone'));
  // The window closed with the browser: kept as a saved window.
  await eventually(async () => {
    expect(shape(await ownTree(sw), s)).toBe('saved_window\n  saved_tab /p/a\n    saved_tab /p/a-child');
  });
  await ctx.close();
  srv.close();
});

test('installing the companion app hands the tree over', async () => {
  const env = bareProfile('handover');
  const srv = await pageServer();
  const s = srv.url;
  let ctx = await launch(env.profile);
  let sw = ctx.serviceWorkers()[0];
  await eventually(async () => expect(await mode(sw)).toBe('alone'));
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  await page.goto(`${s}/p/kept`);
  await eventually(async () => expect(shape(await ownTree(sw), s)).toBe('window\n  tab /p/kept'));
  await new Promise((r) => setTimeout(r, 1000));
  await ctx.close();

  // Now the app is installed, with a new store.
  cli('install-host', '--user-data-dir', env.profile, '--store', env.store, '--host', path.join(BIN, 'treetabs-host'));
  ctx = await launch(env.profile);
  sw = ctx.serviceWorkers()[0];
  await eventually(async () => expect(await mode(sw)).toBe('host'));
  await eventually(() => {
    expect(shape(liveTree(env.store), s)).toBe('saved_window\n  saved_tab /p/kept');
  });
  // The browser's copy goes once the app has it.
  await eventually(async () => expect(await storedKeys(sw)).toEqual([]));
  await ctx.close();
  srv.close();
});

/// What the Settings → Storage button sends, from a tree window page.
async function useApp(ctx, sw, on) {
  let tree = ctx.pages().find((p) => p.url().endsWith('/tree.html'));
  if (!tree) {
    [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
    await tree.waitForLoadState();
  }
  await tree.evaluate((on) => {
    const port = chrome.runtime.connect({ name: 'viewer' });
    port.postMessage(JSON.stringify({ type: 'use_companion_app', on }));
  }, on);
}

test('the companion app can be turned off and on again', async () => {
  const env = setup('switch');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];
  await eventually(async () => expect(await mode(sw)).toBe('host'));
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  await page.goto(`${s}/p/one`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/one'));

  // Off: the tree comes along into the browser.
  await useApp(ctx, sw, false);
  await eventually(async () => expect(await mode(sw)).toBe('alone'));
  await eventually(async () => expect(shape(await ownTree(sw), s)).toBe('window\n  tab /p/one'));
  // Changes now go to the browser's tree only.
  await page.goto(`${s}/p/two`);
  await eventually(async () => expect(shape(await ownTree(sw), s)).toBe('window\n  tab /p/two'));
  expect(shape(liveTree(env.store), s)).not.toContain('/p/two');

  // On again: the tree goes back into the app's store, which did not
  // change meanwhile, so it simply takes it (no backup).
  await useApp(ctx, sw, true);
  await eventually(async () => expect(await mode(sw)).toBe('host'));
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/two'));
  await eventually(async () => expect(await storedKeys(sw)).toEqual([]));
  const backups = fs.existsSync(path.join(env.store, 'backups'))
    ? fs.readdirSync(path.join(env.store, 'backups')).filter((f) => f.startsWith('from-browser-'))
    : [];
  expect(backups).toEqual([]);
  await ctx.close();
  srv.close();
});
