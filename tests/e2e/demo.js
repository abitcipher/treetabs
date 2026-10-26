// Manual check: a browser with the extension and some pages, kept open.
// Usage: node demo.js <seconds>   (prints the store path)
import { setup, launch, pageServer } from './helpers.js';

const env = setup('demo');
const srv = await pageServer();
const ctx = await launch(env.profile);
const page = ctx.pages()[0] ?? (await ctx.newPage());
await page.goto(`${srv.url}/p/docs`);
for (let i = 0; i < 2; i++) {
  const [child] = await Promise.all([ctx.waitForEvent('page'), page.click('#child')]);
  await child.waitForLoadState();
}
const other = await ctx.newPage();
await other.goto(`${srv.url}/p/news`);
console.log(env.store);
await new Promise((r) => setTimeout(r, Number(process.argv[2] ?? 30) * 1000));
await ctx.close();
srv.close();
