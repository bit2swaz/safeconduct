// why does beginFrame make the step count stable?
// same virtual-time loop as spike.mjs on a tiny page both with and without manual frames
// with the page settling on requestAnimationFrame or on setTimeout
// also records when two workers answer: one loaded over the (recorded) network and one from a blob
// run: node frames.mjs [N=30]
import { chromium } from 'playwright';

const N = Number(process.argv[2] ?? 30);
const page = (settle) => `<script>
const msg = (w) => new Promise((ok) => (w.onmessage = (e) => ok(e.data)));
const net = msg(new Worker('/w.js'));
const blob = msg(new Worker(URL.createObjectURL(new Blob(['postMessage(Date.now())']))));
const wait = ${settle === 'raf' ? '() => new Promise(requestAnimationFrame)' : '() => new Promise((ok) => setTimeout(ok, 16))'};
Promise.all([net, blob]).then(async ([n, b]) => {
  await wait(); await wait();
  window.result = { net: n, blob: b, settledAt: performance.now() };
});
</script>`;

async function render(browser, frames, settle) {
  const ctx = await browser.newContext();
  try {
    await ctx.route('**/*', (r) => r.request().url().endsWith('/w.js')
      ? r.fulfill({ contentType: 'text/javascript', body: 'postMessage(Date.now())' })
      : r.fulfill({ contentType: 'text/html', body: page(settle) }));
    const p = await ctx.newPage();
    const cdp = await ctx.newCDPSession(p);
    let ticks = 1000;
    await cdp.send('Emulation.setVirtualTimePolicy', { policy: 'pause', initialVirtualTime: 1700000000 });
    cdp.send('Page.navigate', { url: 'http://site.test/' });
    let steps = 0;
    while (!(await p.evaluate(() => window.result).catch(() => null)) && ++steps <= 300) {
      const expired = new Promise((ok) => cdp.once('Emulation.virtualTimeBudgetExpired', ok));
      await cdp.send('Emulation.setVirtualTimePolicy', { policy: 'pauseIfNetworkFetchesPending', budget: 16 });
      await expired;
      if (frames) await cdp.send('HeadlessExperimental.beginFrame', { frameTimeTicks: (ticks += 16), interval: 16 });
    }
    const r = await p.evaluate(() => window.result).catch(() => null);
    return { steps, ...(r ?? { net: 'TIMEOUT', blob: 'TIMEOUT', settledAt: 'TIMEOUT' }) };
  } finally {
    await ctx.close();
  }
}

console.log(`N = ${N}. cells = unique values (1 = deterministic)\n`);
console.log('| frames | settle on | steps | net worker time | blob worker time | settled at |');
console.log('|---|---|---|---|---|---|');
for (const frames of [true, false]) {
  const browser = await chromium.launch({ args: frames ? ['--deterministic-mode'] : [] });
  for (const settle of ['raf', 'timeout']) {
    const seen = { steps: new Set(), net: new Set(), blob: new Set(), settledAt: new Set() };
    for (let i = 0; i < N; i++) {
      const r = await render(browser, frames, settle);
      for (const k in seen) seen[k].add(String(r[k]));
    }
    const cell = (s) => `${s.size} (${[...s].slice(0, 3).join(', ')}${s.size > 3 ? ', ...' : ''})`;
    console.log(`| ${frames ? 'beginFrame' : 'chrome'} | ${settle === 'raf' ? 'rAF' : 'setTimeout'} | ${cell(seen.steps)} | ${cell(seen.net)} | ${cell(seen.blob)} | ${cell(seen.settledAt)} |`);
  }
  await browser.close();
}
