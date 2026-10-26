// Runs only with SNAP_TEST=1: the Ubuntu snap Chromium, with a throwaway
// profile under ~/snap/chromium/common/treetabs-test (snap confinement only
// lets the browser run binaries and reach files there).
import { test, expect, chromium } from '@playwright/test';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { cli, EXT, ROOT, TEST_ENV, liveTree, shape, eventually, pageServer } from './helpers.js';

test.skip(!process.env.SNAP_TEST, 'set SNAP_TEST=1');

// Snap browsers to try (SNAPS=chromium,brave,opera; default chromium).
const SNAPS = (process.env.SNAPS ?? 'chromium').split(',');

for (const snap of SNAPS)
for (const serverFirst of snap === 'chromium' ? [false, true] : [false])
test(`snap ${snap} reaches the native host (server outside the snap first: ${serverFirst})`, async () => {
  const base = path.join(os.homedir(), `snap/${snap}/common/treetabs-test`);
  fs.rmSync(base, { recursive: true, force: true });
  const profile = path.join(base, 'profile');
  const store = path.join(base, 'store');
  fs.mkdirSync(profile, { recursive: true });
  fs.copyFileSync(path.join(ROOT, 'target/debug/treetabs-host'), path.join(base, 'treetabs-host'));
  fs.chmodSync(path.join(base, 'treetabs-host'), 0o755);
  // The extension must also be readable by the confined browser.
  fs.cpSync(EXT, path.join(base, 'ext'), { recursive: true });
  // Brave and Opera only read hosts from their snap home: register there
  // for the test, and take it away afterwards unless it was already there.
  const homeHosts = {
    brave: 'snap/brave/current/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts',
    opera: 'snap/opera/current/.config/google-chrome/NativeMessagingHosts',
  }[snap];
  const hostFile = homeHosts && path.join(os.homedir(), homeHosts, 'dev.treetabs.host.json');
  const hadHost = hostFile && fs.existsSync(hostFile);
  const snapDir = path.join(os.homedir(), `snap/${snap}/common/treetabs`);
  const hadSnapDir = fs.existsSync(snapDir);
  if (hostFile) {
    if (hadHost) fs.copyFileSync(hostFile, `${hostFile}.bak`);
    cli('install-host', '--browser', `${snap}-snap`, '--store', store, '--host', path.join(base, 'treetabs-host'));
  } else {
    cli('install-host', '--user-data-dir', profile, '--store', store, '--host', path.join(base, 'treetabs-host'));
  }

  // Like the GUI starting before the browser: an unconfined process owns
  // the store and the confined host has to connect to its socket.
  const serve = serverFirst ? spawn(path.join(ROOT, 'target/debug/treetabs'), ['serve', '--store', store], { env: TEST_ENV }) : null;
  if (serve) await eventually(() => expect(fs.existsSync(path.join(store, 'server.sock'))).toBe(true));
  const srv = await pageServer();
  const ctx = await chromium.launchPersistentContext(profile, {
    executablePath: `/snap/bin/${snap}`,
    headless: true,
    env: TEST_ENV,
    args: [`--disable-extensions-except=${base}/ext`, `--load-extension=${base}/ext`],
  });
  if (ctx.serviceWorkers().length === 0) await ctx.waitForEvent('serviceworker');
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  await page.goto(`${srv.url}/p/snap`);
  await eventually(() => expect(shape(liveTree(store), srv.url)).toBe('window\n  tab /p/snap'), { timeout: 20000 });
  await ctx.close();
  srv.close();
  serve?.kill();
  if (hostFile) {
    if (hadHost) fs.renameSync(`${hostFile}.bak`, hostFile);
    else fs.rmSync(hostFile, { force: true });
    if (!hadSnapDir) fs.rmSync(snapDir, { recursive: true, force: true });
  }
});
