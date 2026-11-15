// A real Stagehand v4 agent on the replayed shop. The model is Gemini (free tier), via GEMINI_API_KEY.
// Stagehand runs act()/extract() inside its Chrome extension, which calls the model from the browser.
// SC_RULES=<recording>: start the runtime here through the TS client, with the built-in rules,
// and also pay. Then it prints {answer, code, steps, violations, requests}.
import { readFileSync } from 'node:fs';
import { localBrowser, Stagehand } from '@browserbasehq/stagehand';
import { z } from 'zod/v4';
import { allowedOrigins, noDoublePayment, replay } from '../clients/ts/index.ts';
const steps = JSON.parse(readFileSync(new URL('steps.json', import.meta.url)));

const rules = [allowedOrigins([new URL(steps.url).origin]), noDoublePayment('/api/pay')];
const run = process.env.SC_RULES ? await replay(process.env.SC_RULES, { liveModel: true, rules }) : null;
const browser = await localBrowser.connect({ cdpUrl: run?.endpoint ?? 'http://127.0.0.1:9222' });
const stagehand = await Stagehand.create({
  browser,
  model: { modelName: `google/${process.env.GEMINI_MODEL}`, apiKey: process.env.GEMINI_API_KEY },
});
// Waits are bounded: under fuzz faults (a cookie wall over the button) the cart may never change,
// and the agent must still report what it sees.
const until = async (expr, ms) => {
  for (const end = Date.now() + ms; Date.now() < end; await new Promise((r) => setTimeout(r, 100))) {
    if (await page.evaluate(expr)) return true;
  }
  return false;
};
let page, out = {};
try {
  page = await browser.context.newPage();
  await page.goto(steps.url);
  await until(steps.ready, 30000);
  await stagehand.act('add the cheapest product to the cart');
  await until(steps.added, 10000);
  if (run) {
    await stagehand.act('pay for the order');
    await until(steps.paid, 10000);
  }
  const { data } = await stagehand.extract('the cart count and the order number shown on the page', z.object({ count: z.number(), order: z.string() }));
  out = { answer: JSON.stringify(data) };
} catch (e) {
  out = { answer: null, error: String(e.message ?? e).slice(0, 300) };
}
await stagehand.close();
await browser.disconnect();
if (run) {
  const code = await run.close();
  out = { ...out, code, steps: run.steps, violations: run.violations, requests: run.events.filter((e) => e.event === 'request') };
}
console.log(JSON.stringify(out));
