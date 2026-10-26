import { test, expect } from '@playwright/test';
import { setup, launch, liveTree, eventually, pageServer } from './helpers.js';

/// Outline of the "treetabs" bookmark folder, read inside the extension.
async function folder(sw, server) {
  return sw.evaluate(async (server) => {
    const [root] = await chrome.bookmarks.getTree();
    const other = root.children.find((c) => c.folderType === 'other') ?? root.children[1];
    const ours = other.children.find((c) => c.title === 'treetabs' && !c.url);
    if (!ours) return null;
    const lines = [];
    const walk = (n, d) => {
      for (const c of n.children ?? []) {
        if (c.url && !c.url.startsWith(server)) continue;
        lines.push('  '.repeat(d) + (c.url ? `${c.title} <${c.url.slice(server.length)}>` : `[${c.title}]`));
        walk(c, d + 1);
      }
    };
    walk(ours, 0);
    return { id: ours.id, text: lines.join('\n') };
  }, server);
}

test('tree is mirrored to bookmarks and bookmark edits come back', async () => {
  const env = setup('bookmarks');
  const srv = await pageServer();
  const s = srv.url;
  const ctx = await launch(env.profile);
  const sw = ctx.serviceWorkers()[0];

  const page = ctx.pages()[0] ?? (await ctx.newPage());
  await page.goto(`${s}/p/a`);
  const [child] = await Promise.all([ctx.waitForEvent('page'), page.click('#child')]);
  await child.waitForLoadState();

  await eventually(async () => {
    const f = await folder(sw, s);
    expect(f?.text).toBe('[Window]\n  [a]\n    a </p/a>\n    a-child </p/a-child>');
  });

  // The user renames the window folder in the bookmark manager.
  await sw.evaluate(async () => {
    const [root] = await chrome.bookmarks.getTree();
    const other = root.children.find((c) => c.folderType === 'other') ?? root.children[1];
    const ours = other.children.find((c) => c.title === 'treetabs');
    const win = ours.children.find((c) => c.title === 'Window');
    await chrome.bookmarks.update(win.id, { title: 'Research' });
  });
  await eventually(() => {
    const t = liveTree(env.store);
    const w = t.root.children.find((c) => c.kind === 'window' && c.children?.some((x) => x.url === `${s}/p/a`));
    expect(w.custom_title).toBe('Research');
  });

  // Closing the child tab removes its bookmark (and the tab's folder).
  await child.close();
  await eventually(async () => {
    const f = await folder(sw, s);
    expect(f?.text).toBe('[Research]\n  a </p/a>');
  });

  await ctx.close();
  srv.close();
});
