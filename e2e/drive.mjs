// Drive the shop through Playwright or Stagehand, attached over CDP like any agent would.
// Usage: node drive.mjs playwright|stagehand [--probe] [--fuzz]   -> prints {"text", "probe"}
// --fuzz: the page may never get ready (a fault dropped its data): wait 4s at most, never throw.
import { readFileSync } from 'node:fs';
const steps = JSON.parse(readFileSync(new URL('steps.json', import.meta.url)));
const [client, probe, fuzz] = [process.argv[2], process.argv.includes('--probe'), process.argv.includes('--fuzz')];
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
  for (let i = 0; i < (fuzz ? 40 : 100); i++) { if (await js(expr)) return true; await new Promise((r) => setTimeout(r, 100)); }
  if (!fuzz) throw new Error('timed out: ' + expr);
  return false;
};
await page.goto(steps.url);
if (await until(steps.ready)) {
  await js(steps.addCheapest);
  if (await until(steps.added)) {
    await js(steps.pay);
    await until(steps.paid);
  }
}
const text = await js(steps.text);
// In exact mode the probe is off the recording, so the runtime ends the run right after it.
console.log(JSON.stringify({ text, probe: probe ? await js(steps.probe) : null }));
await Promise.race([done(), new Promise((r) => setTimeout(r, 2000))]).catch(() => {});
process.exit(0);
