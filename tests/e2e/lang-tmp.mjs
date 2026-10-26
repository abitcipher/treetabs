import { setup, launch, pageServer } from './helpers.js';
const out = process.argv[2];
const langs = process.argv.slice(3);
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const env = setup('lang');
const srv = await pageServer();
const ctx = await launch(env.profile);
const page = ctx.pages()[0];
await page.goto(`${srv.url}/p/docs`);
const [child] = await Promise.all([ctx.waitForEvent('page'), page.click('#child')]);
await child.waitForLoadState();
const sw = ctx.serviceWorkers()[0];
const [tree] = await Promise.all([ctx.waitForEvent('page'), sw.evaluate(() => treetabsOpenTree())]);
await tree.setViewportSize({ width: 1100, height: 520 });
for (const l of langs) {
  await tree.evaluate((l) => localStorage.setItem('treetabs.ui', JSON.stringify({ language: l })), l);
  await tree.reload();
  await pause(3000);
  await tree.mouse.click(70, 82);
  await tree.mouse.click(70, 82, { button: 'right' });
  await pause(400);
  await tree.screenshot({ path: `${out}/lang-${l}-menu.png` });
  await tree.keyboard.press('Escape');
  await tree.mouse.click(1085, 18);
  await pause(400);
  await tree.screenshot({ path: `${out}/lang-${l}-settings.png` });
}
await ctx.close(); srv.close();
