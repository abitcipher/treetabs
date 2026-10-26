import { test, expect } from '@playwright/test';
import { setup, launch, liveTree, shape, eventually, pageServer, cli } from './helpers.js';

test('tabs are tracked, nested by opener, trashed on close, saved after restart', async () => {
  const env = setup('tabs');
  const srv = await pageServer();
  const s = srv.url;

  let ctx = await launch(env.profile);
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  await page.goto(`${s}/p/a`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/a'));

  // A link opened from page a becomes its child.
  const [child] = await Promise.all([ctx.waitForEvent('page'), page.click('#child')]);
  await child.waitForLoadState();
  await eventually(() =>
    expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/a\n    tab /p/a-child'),
  );

  // Closing the child tab moves it to the trash.
  await child.close();
  await eventually(() => {
    const t = liveTree(env.store);
    expect(shape(t, s)).toBe('window\n  tab /p/a');
    expect(t.trash.map((x) => x.node.url)).toContain(`${s}/p/a-child`);
  });

  // Quit and start again: the old window must be a saved window holding
  // tab a only (the child stays in the trash).
  await ctx.close();
  ctx = await launch(env.profile);
  await eventually(() => {
    const t = liveTree(env.store);
    expect(shape(t, s)).toBe('saved_window\n  saved_tab /p/a');
    const w = t.root.children.find((c) => c.kind === 'saved_window');
    // Saved either from the close events or by the restart check.
    expect(w.data.closeDate ?? w.data.crashDetectedDate).toBeTruthy();
  });
  await ctx.close();
  srv.close();
});

test('a saved window opened from the tree becomes that window again', async () => {
  const env = setup('reopen');
  const srv = await pageServer();
  const s = srv.url;
  let ctx = await launch(env.profile);
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  await page.goto(`${s}/p/x`);
  const p2 = await ctx.newPage();
  await p2.goto(`${s}/p/y`);
  await eventually(() => expect(shape(liveTree(env.store), s)).toBe('window\n  tab /p/x\n  tab /p/y'));
  await ctx.close();
  ctx = await launch(env.profile);
  let saved;
  await eventually(() => {
    const t = liveTree(env.store);
    expect(shape(t, s)).toBe('saved_window\n  saved_tab /p/x\n  saved_tab /p/y');
    saved = t.root.children.find((c) => c.kind === 'saved_window' && c.children?.some((x) => x.url === `${s}/p/x`));
  });
  cli('open', String(saved.id), '--store', env.store);
  await eventually(() => {
    const t = liveTree(env.store);
    // Same node, now open, with both tabs bound (no duplicates).
    expect(shape(t, s)).toBe('window\n  tab /p/x\n  tab /p/y');
    expect(t.root.children.find((c) => c.id === saved.id)?.kind).toBe('window');
  });
  await ctx.close();
  srv.close();
});
