// Runs only with FIREFOX_TEST=1: the Firefox add-on without the companion
// app, through the snap Firefox's geckodriver. No host may be registered
// for Firefox (~/.mozilla/native-messaging-hosts/dev.treetabs.host.json);
// the test is skipped when one is.
import { test, expect } from '@playwright/test';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';

const UUID = '7c1e4b2a-9d3f-4e5a-8b6c-1f2e3d4c5b6a';
const base = path.join(os.homedir(), 'snap/firefox/common/treetabs-alone-test');
const ext = path.join(base, 'ext');
const profile = path.join(base, 'profile');
const hostFile = path.join(os.homedir(), '.mozilla/native-messaging-hosts/dev.treetabs.host.json');
test.skip(!process.env.FIREFOX_TEST || fs.existsSync(hostFile), 'set FIREFOX_TEST=1, with no Firefox host registered');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const check = (name, cond, extra = '') => expect(!!cond, `${name} ${extra}`).toBe(true);

test('firefox without the companion app: tracks tabs, stays running, keeps the tree', async () => {
  test.setTimeout(240_000); // it waits 70 s on purpose
fs.rmSync(base, { recursive: true, force: true });
fs.mkdirSync(profile, { recursive: true });
fs.cpSync(path.join(import.meta.dirname, '../../extension/dist/firefox'), ext, { recursive: true });
const srv = http.createServer((q, r) => {
  const n = q.url.replace(/^\/p\//, '');
  r.setHeader('content-type', 'text/html');
  r.end(`<title>${n}</title><a id="child" href="/p/${n}-child" target="_blank">child</a>`);
});
await new Promise((r) => srv.listen(0, '127.0.0.1', r));
const S = `http://127.0.0.1:${srv.address().port}`;

const gd = spawn('/snap/bin/firefox.geckodriver', ['--port', '4456', '--allow-system-access'], { stdio: 'ignore' });
await sleep(1500);
const wd = async (method, p, body) => {
  const r = await fetch(`http://127.0.0.1:4456${p}`, { method, headers: { 'content-type': 'application/json' }, body: body && JSON.stringify(body) });
  const j = await r.json();
  if (j.value?.error) throw new Error(`${p}: ${j.value.error} ${j.value.message}`);
  return j.value;
};
let sid;
async function start() {
  const prefs = {
    'extensions.webextensions.uuids': JSON.stringify({ 'treetabs@treetabs.dev': UUID }),
    // A temporary add-on is removed with the session: keep its storage.
    'extensions.webextensions.keepStorageOnUninstall': true,
    'extensions.webextensions.keepUuidOnUninstall': true,
  };
  sid = (await wd('POST', '/session', { capabilities: { alwaysMatch: { 'moz:firefoxOptions': { args: ['-headless', '-profile', profile], prefs } } } })).sessionId;
  await wd('POST', `/session/${sid}/moz/addon/install`, { path: ext, temporary: true });
  await wd('POST', `/session/${sid}/window/rect`, { width: 1100, height: 700 });
}
async function openTree() {
  await wd('POST', `/session/${sid}/moz/context`, { context: 'chrome' });
  await wd('POST', `/session/${sid}/execute/sync`, {
    script: `const w = Services.wm.getMostRecentWindow('navigator:browser');
      w.gBrowser.selectedTab = w.gBrowser.addTab(arguments[0], { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() });`,
    args: [`moz-extension://${UUID}/tree.html`],
  });
  await wd('POST', `/session/${sid}/moz/context`, { context: 'content' });
  const handles = await wd('GET', `/session/${sid}/window/handles`);
  await wd('POST', `/session/${sid}/window`, { handle: handles[handles.length - 1] });
  await sleep(2500);
}
async function closeTab() {
  await wd('DELETE', `/session/${sid}/window`);
  const handles = await wd('GET', `/session/${sid}/window/handles`);
  await wd('POST', `/session/${sid}/window`, { handle: handles[0] });
}
// In the tree page: the background's mode, and the tree from a viewer connection.
async function state() {
  return wd('POST', `/session/${sid}/execute/async`, {
    script: `const done = arguments[arguments.length - 1];
      (async () => {
        const bg = await browser.runtime.getBackgroundPage();
        const port = browser.runtime.connect({ name: 'viewer' });
        const got = [];
        port.onMessage.addListener((m) => got.push(JSON.parse(m)));
        for (let i = 0; i < 50 && !got.some((m) => m.type === 'store_options'); i++) await new Promise((r) => setTimeout(r, 100));
        port.disconnect();
        const snap = got.find((m) => m.type === 'snapshot');
        const opts = got.find((m) => m.type === 'store_options');
        const status = got.filter((m) => m.type === 'status').map((m) => m.message);
        const app = got.find((m) => m.type === 'companion_app')?.exists ?? null;
        const keys = Object.keys(await browser.storage.local.get(null)).filter((k) => k.startsWith('tt.'));
        done({ mode: bg?.treetabsMode?.() ?? null, probe: bg?.__probe ?? null, kind: opts?.kind ?? null, tree: snap?.tree ?? null, status, keys: keys.length, app });
      })().catch((e) => done({ error: String(e) }));`,
    args: [],
  });
}
async function setProbe() {
  await wd('POST', `/session/${sid}/execute/async`, {
    script: `const done = arguments[arguments.length - 1];
      browser.runtime.getBackgroundPage().then((bg) => { bg.__probe = 42; done(true); });`,
    args: [],
  });
}
function lines(tree) {
  const out = [];
  const go = (n, d) => { for (const c of n.children ?? []) { const u = c.url?.startsWith(S) ? ' ' + c.url.slice(S.length) : ''; if (u || /window/.test(c.kind)) out.push('  '.repeat(d) + c.kind + u); go(c, d + 1); } };
  go(tree.root, 0);
  return out.join('\n');
}

try {
  await start();
  await wd('POST', `/session/${sid}/url`, { url: `${S}/p/fox` });
  await wd('POST', `/session/${sid}/element/${Object.values(await wd('POST', `/session/${sid}/element`, { using: 'css selector', value: '#child' }))[0]}/click`, {});
  await sleep(1500);
  await openTree();
  let st = await state();
  if (st.error) console.log('ERROR', st.error);
  // console.log('INFO mode', st.mode, 'kind', st.kind, 'status', JSON.stringify(st.status));
  check('Firefox runs without the app', st.mode === 'alone' && st.kind === 'browser');
  // On Linux the tree window offers the app (Firefox gives the system too).
  check('the app is offered on Linux', st.app === true, String(st.app));
  check('tabs are tracked and nested', st.tree && lines(st.tree).includes('tab /p/fox\n    tab /p/fox-child'), st.tree ? '\n' + lines(st.tree) : '');

  // Idle with no add-on page open: does the background stay?
  await setProbe();
  await closeTab();
  // console.log('INFO idle 70 s, no add-on page open…');
  await sleep(70000);
  await openTree();
  st = await state();
  check('the background stays running when idle', st.probe === 42, `probe=${st.probe}`);
  await closeTab();

  // A tab closed: to the trash.
  const handles = await wd('GET', `/session/${sid}/window/handles`);
  await wd('POST', `/session/${sid}/window`, { handle: handles[handles.length - 1] });
  const url = await wd('GET', `/session/${sid}/url`);
  if (url.endsWith('-child')) await closeTab();
  await sleep(1000);
  await openTree();
  st = await state();
  check('a closed tab goes to the trash', (st.tree?.trash ?? []).some((t) => t.node.url === `${S}/p/fox-child`));
  check('it is saved in the browser storage', st.keys > 2, `keys=${st.keys}`);

  // Restart Firefox with the same profile, as if on Windows from now on.
  await wd('POST', `/session/${sid}/execute/async`, {
    script: `const done = arguments[arguments.length - 1];
      browser.storage.local.set({ testPlatform: 'win' }).then(() => done(true));`,
    args: [],
  });
  await wd('DELETE', `/session/${sid}`);
  sid = null;
  await sleep(1000);
  await start();
  await sleep(2000);
  await openTree();
  st = await state();
  // console.log('INFO after restart mode', st.mode, '\n' + (st.tree ? lines(st.tree) : st.error));
  check('after a restart the tree is still there', st.tree && lines(st.tree).includes('/p/fox'));
  check('after a restart the trash is kept', (st.tree?.trash ?? []).some((t) => t.node.url === `${S}/p/fox-child`));
  check('on Windows the app is not offered', st.mode === 'alone' && st.app === false, `${st.mode} ${st.app}`);

  // Double-click a saved tab (what the tree window sends): it opens, and
  // the saved node becomes that tab, without a second node.
  const savedTab = (() => { let f; const go = (n) => { for (const c of n.children ?? []) { if (c.url === `${S}/p/fox` && c.kind === 'saved_tab') f = c; go(c); } }; go(st.tree.root); return f; })();
  check('there is a saved tab to open', !!savedTab);
  await wd('POST', `/session/${sid}/execute/async`, {
    script: `const done = arguments[arguments.length - 1];
      const port = browser.runtime.connect({ name: 'viewer' });
      port.onMessage.addListener((m) => { if (JSON.parse(m).type === 'snapshot') { port.postMessage(JSON.stringify({ type: 'activate', id: arguments[0] })); setTimeout(() => { port.disconnect(); done(true); }, 500); } });`,
    args: [savedTab.id],
  });
  await sleep(3000);
  st = await state();
  const fox = [];
  const walk = (n) => { for (const c of n.children ?? []) { if (c.url === `${S}/p/fox`) fox.push(c); walk(c); } };
  walk(st.tree.root);
  check('the saved tab opens as itself, with no second node', fox.length === 1 && fox[0].id === savedTab.id && fox[0].kind === 'tab', JSON.stringify(fox.map((n) => [n.id, n.kind])));
} finally {
  if (sid) await wd('DELETE', `/session/${sid}`).catch(() => {});
  gd.kill();
  srv.close();
  fs.rmSync(base, { recursive: true, force: true });
}
});
