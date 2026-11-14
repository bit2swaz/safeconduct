// A real Stagehand v4 agent on the replayed shop. The model is Gemini (free tier), via GEMINI_API_KEY.
// Stagehand runs act()/extract() inside its Chrome extension, which calls the model from the browser.
import { readFileSync } from 'node:fs';
import { localBrowser, Stagehand } from '@browserbasehq/stagehand';
import { z } from 'zod/v4';
const steps = JSON.parse(readFileSync(new URL('steps.json', import.meta.url)));

const browser = await localBrowser.connect({ cdpUrl: 'http://127.0.0.1:9222' });
const stagehand = await Stagehand.create({
  browser,
  model: { modelName: `google/${process.env.GEMINI_MODEL}`, apiKey: process.env.GEMINI_API_KEY },
});
const page = await browser.context.newPage();
await page.goto(steps.url);
await page.waitForFunction(steps.ready);
await stagehand.act('add the cheapest product to the cart');
await page.waitForFunction(steps.added);
const { data } = await stagehand.extract('the cart count and the order number shown on the page', z.object({ count: z.number(), order: z.string() }));
console.log(JSON.stringify({ answer: JSON.stringify(data) }));
await stagehand.close();
await browser.disconnect();
