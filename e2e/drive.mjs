// Drive the shop through Playwright or Stagehand, attached over CDP like any agent would.
// Usage: node drive.mjs playwright|stagehand [--probe]   -> prints {"text", "probe"}
import { readFileSync } from 'node:fs';
const steps = JSON.parse(readFileSync(new URL('steps.json', import.meta.url)));
const [client, probe] = [process.argv[2], process.argv.includes('--probe')];
const CDP = 'http://127.0.0.1:9222';

let page, done;
if (client === 'playwright') {
  const { chromium } = await import('playwright-core');
  const browser = await chromium.connectOverCDP(CDP);
  page = await browser.contexts()[0].newPage();
  done = () => browser.close();
} else {
  const { localBrowser, Stagehand } = await import('@browserbasehq/stagehand');
  const browser = await localBrowser.connect({ cdpUrl: CDP });
  const stagehand = await Stagehand.create({ browser }); // no model: we only drive the page
  page = await browser.context.newPage();
  done = async () => { await stagehand.close?.(); await browser.disconnect(); };
}
const js = (expr) => page.evaluate(expr);
const until = async (expr) => {
  for (let i = 0; i < 100; i++) { if (await js(expr)) return; await new Promise((r) => setTimeout(r, 100)); }
  throw new Error('timed out: ' + expr);
};
await page.goto(steps.url);
await until(steps.ready);
await js(steps.addCheapest);
await until(steps.added);
console.log(JSON.stringify({ text: await js(steps.text), probe: probe ? await js(steps.probe) : null }));
await done();
