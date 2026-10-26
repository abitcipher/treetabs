// Runs only with FIREFOX_TEST=1: the Ubuntu snap Firefox through its
// geckodriver, with the add-on installed temporarily. Snap Firefox starts
// native hosts through the desktop portal, which asks the user once; the
// test grants that for dev.treetabs.host and takes it back afterwards.
import { test, expect } from '@playwright/test';
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { cli, ROOT, liveTree, shape, eventually, pageServer } from './helpers.js';

test.skip(!process.env.FIREFOX_TEST, 'set FIREFOX_TEST=1');

const SHOTS = process.env.SHOTS;
const UUID = '0e7f3a52-4d1b-4c41-9d0e-6a1d2f9b7c10';
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const store_ = (method, ...args) =>
  execFileSync('gdbus', ['call', '--session', '--dest', 'org.freedesktop.impl.portal.PermissionStore', '--object-path',
    '/org/freedesktop/impl/portal/PermissionStore', '--method', `org.freedesktop.impl.portal.PermissionStore.${method}`, ...args], { encoding: 'utf8' });

test('firefox: the add-on reaches the host, and the tree window shows the tree', async () => {
  const base = path.join(os.homedir(), 'snap/firefox/common/treetabs-test');
  fs.rmSync(base, { recursive: true, force: true });
  fs.mkdirSync(base, { recursive: true });
  fs.cpSync(path.join(ROOT, 'extension/dist/firefox'), path.join(base, 'ext'), { recursive: true });
  const store = path.join(base, 'store');
  const hostFile = path.join(os.homedir(), '.mozilla/native-messaging-hosts/dev.treetabs.host.json');
  const had = fs.existsSync(hostFile);
  if (had) fs.copyFileSync(hostFile, `${hostFile}.bak`);
  const hadPermission = store_('List', 'webextensions').includes('dev.treetabs.host');
  cli('install-host', '--browser', 'firefox', '--store', store, '--host', path.join(ROOT, 'target/debug/treetabs-host'));
  if (!hadPermission) store_('SetPermission', 'webextensions', 'true', 'dev.treetabs.host', 'snap.firefox', "['yes']");
  const gd = spawn('/snap/bin/firefox.geckodriver', ['--port', '4455', '--allow-system-access'], { stdio: 'ignore' });
  await pause(1500);
  const wd = async (method, p, body) => {
    const r = await fetch(`http://127.0.0.1:4455${p}`, { method, headers: { 'content-type': 'application/json' }, body: body && JSON.stringify(body) });
    const j = await r.json();
    if (j.value?.error) throw new Error(`${p}: ${j.value.error} ${j.value.message}`);
    return j.value;
  };
  let sid;
  const srv = await pageServer();
  try {
    const prefs = { 'extensions.webextensions.uuids': JSON.stringify({ 'treetabs@treetabs.dev': UUID }) };
    sid = (await wd('POST', '/session', { capabilities: { alwaysMatch: { 'moz:firefoxOptions': { args: ['-headless'], prefs } } } })).sessionId;
    await wd('POST', `/session/${sid}/moz/addon/install`, { path: path.join(base, 'ext'), temporary: true });
    await wd('POST', `/session/${sid}/url`, { url: `${srv.url}/p/fox` });
    await eventually(() => expect(shape(liveTree(store), srv.url)).toBe('window\n  tab /p/fox'), { timeout: 20000 });

    // The tree page, opened from the browser's own context (WebDriver may
    // not navigate to add-on pages).
    await wd('POST', `/session/${sid}/window/rect`, { width: 1100, height: 700 });
    await wd('POST', `/session/${sid}/moz/context`, { context: 'chrome' });
    await wd('POST', `/session/${sid}/execute/sync`, {
      script: `const w = Services.wm.getMostRecentWindow('navigator:browser');
        w.gBrowser.selectedTab = w.gBrowser.addTab(arguments[0], { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() });`,
      args: [`moz-extension://${UUID}/tree.html`],
    });
    await wd('POST', `/session/${sid}/moz/context`, { context: 'content' });
    await eventually(() => expect(fs.readFileSync(path.join(store, 'host.log'), 'utf8')).toMatch(/tree window \(Viewer\)/), { timeout: 15000 });
    await pause(3000);
    if (SHOTS) {
      const handles = await wd('GET', `/session/${sid}/window/handles`);
      await wd('POST', `/session/${sid}/window`, { handle: handles[handles.length - 1] });
      fs.writeFileSync(`${SHOTS}/ff-tree.png`, Buffer.from(await wd('GET', `/session/${sid}/screenshot`), 'base64'));
    }
    // Its own tab is not part of the tree.
    expect(shape(liveTree(store), srv.url)).toBe('window\n  tab /p/fox');
  } finally {
    srv.close();
    if (sid) await wd('DELETE', `/session/${sid}`).catch(() => {});
    gd.kill();
    if (had) fs.renameSync(`${hostFile}.bak`, hostFile);
    else fs.rmSync(hostFile, { force: true });
    if (!hadPermission) store_('Delete', 'webextensions', 'dev.treetabs.host');
  }
});
