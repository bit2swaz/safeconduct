// A naive buyer under fuzz faults, for `safeconduct shrink`. It retries a failed payment with
// the same order, so a payment fault makes it pay twice. Exit 1 = a rule broke, else exit 0.
// Usage: node shrink_test.mjs <recording> <seed> <fault-rate>   -> prints {"violations", "fingerprint"}
import { readFileSync } from 'node:fs';
import { chromium } from 'playwright-core';
import { noDoublePayment, replay } from '../clients/ts/index.ts';
const steps = JSON.parse(readFileSync(new URL('steps.json', import.meta.url)));
const [file, seed, rate] = process.argv.slice(2);

const run = await replay(file, { fuzz: Number(seed), faultRate: Number(rate), rules: [noDoublePayment('/api/pay')] });
const browser = await chromium.connectOverCDP(run.endpoint);
const page = await browser.contexts()[0].newPage();
const until = (expr) => page.waitForFunction(expr, null, { timeout: 5000 }).then(() => true, () => false);
try {
  await page.goto(steps.url);
  if ((await until(steps.ready)) && (await page.evaluate(steps.addCheapest)) && (await until(steps.added))) {
    for (let i = 0; i < 3; i++) {
      await page.evaluate("document.querySelector('#paid').textContent = ''");
      await page.evaluate(steps.pay);
      await until(steps.paid);
      if ((await page.evaluate("document.querySelector('#paid').textContent")).startsWith('paid')) break;
    }
  }
} catch {} // a fault can break the page in other ways: only the rules decide the result
await browser.close().catch(() => {});
await run.close();
console.log(JSON.stringify({ violations: run.violations, fingerprint: run.fingerprint }));
process.exit(run.violations.length ? 1 : 0);
