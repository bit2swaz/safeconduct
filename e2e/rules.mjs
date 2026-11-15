// The TS client's rules on a replayed shop, driven by Playwright.
// Usage: node rules.mjs <recording> clean|bad   -> prints {"code", "steps", "violations"}
// clean: buy the cheapest product once. bad: pay twice, then leave the shop's origin.
import { readFileSync } from 'node:fs';
import { chromium } from 'playwright-core';
import { allowedOrigins, noDoublePayment, replay } from '../clients/ts/index.ts';
const steps = JSON.parse(readFileSync(new URL('steps.json', import.meta.url)));
const [file, mode] = process.argv.slice(2);

const run = await replay(file, { rules: [allowedOrigins([new URL(steps.url).origin]), noDoublePayment('/api/pay')] });
const browser = await chromium.connectOverCDP(run.endpoint);
const page = await browser.contexts()[0].newPage();
const until = (expr) => page.waitForFunction(expr, null, { timeout: 10000 });
await page.goto(steps.url);
await until(steps.ready);
await page.evaluate(steps.addCheapest);
await until(steps.added);
await page.evaluate(steps.pay);
await until(steps.paid);
if (mode === 'bad') {
  await page.evaluate(steps.pay);
  await page.waitForTimeout(500);
  await page.goto('http://127.0.0.1:8002/').catch(() => {}); // off the recording too: the run ends
}
await browser.close().catch(() => {});
const code = await run.close();
console.log(JSON.stringify({ code, steps: run.steps, violations: run.violations }));
