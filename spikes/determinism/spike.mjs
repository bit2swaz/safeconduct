// B0: can a browser be deterministic?
// render one page N times per config. hash the DOM, the a11y tree and the screenshot
// turn each fix off one at a time and count unique hashes (the ablation table)
// run: node spike.mjs [N=100]
import http from 'node:http';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { chromium } from 'playwright';

const N = Number(process.argv[2] ?? 100);
const PORT = 8790;
const ORIGIN = `http://localhost:${PORT}`;

// the page uses every nondeterminism source from L0
const files = {
  '/': `<!doctype html><title>shop</title>
<style>
  body { font: 16px sans-serif; margin: 16px; }
  .spin { width: 24px; height: 24px; border: 4px solid #ccc; border-top-color: #333; border-radius: 50%;
          animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
<h1>shop</h1>
<p>now: <span id=now></span></p>
<p>session: <span id=session></span></p>
<p>pick for you: <span id=pick></span></p>
<p>lucky (worker): <span id=lucky></span></p>
<ul id=items></ul>
<div class=spin role=progressbar aria-label=loading></div>
<canvas id=chart width=240 height=60></canvas>
<p id=promo hidden>promo: free shipping</p>
<script>
const names = ['lamp', 'mug', 'chair', 'desk', 'plant'];
now.textContent = new Date().toISOString();
session.textContent = crypto.randomUUID();
pick.textContent = names[Math.floor(Math.random() * names.length)];
const ctx = chart.getContext('2d');
ctx.font = '14px sans-serif';
ctx.fillText('sales ' + crypto.getRandomValues(new Uint8Array(1))[0], 8, 36);
const add = (t) => { const li = document.createElement('li'); li.textContent = t; items.append(li); };
const net = Promise.all([
  fetch('/api/products').then((r) => r.json()).then((ps) => ps.forEach(add)),
  fetch('/api/deal').then((r) => r.text()).then(add), // arrival order decides list order
]);
const worker = new Promise((ok) => {
  const w = new Worker('/worker.js');
  w.onmessage = (e) => { lucky.textContent = e.data; ok(); };
});
const promo = new Promise((ok) => setTimeout(() => { document.getElementById('promo').hidden = false; ok(); }, 300));
const frame = () => new Promise(requestAnimationFrame);
Promise.all([net, worker, promo]).then(frame).then(frame).then(() => { window.done = true; });
</script>`,
  '/worker.js': `postMessage(Math.floor(Math.random() * 1000) + ' @ ' + Date.now());`,
  '/api/products': JSON.stringify(['lamp $20', 'mug $8']),
};

// live site: random latency on every request and the deal changes every call
const server = http.createServer((req, res) => {
  const body = req.url === '/api/deal' ? `deal $${(Math.random() * 100).toFixed(2)}` : files[req.url];
  if (body === undefined) { res.statusCode = 404; return res.end(); }
  const type = req.url.endsWith('.js') ? 'text/javascript' : req.url.startsWith('/api/products') ? 'application/json' : 'text/html';
  setTimeout(() => res.writeHead(200, { 'content-type': type }).end(body), Math.random() * 30);
});
await new Promise((ok) => server.listen(PORT, ok));

// recording: fetch every url once from the live site and save the bytes to recording.json
// later runs (and other machines) load that file so they all replay the same "saved page"
const REC = new URL('./recording.json', import.meta.url);
if (!existsSync(REC)) {
  const rec = {};
  for (const path of [...Object.keys(files), '/api/deal']) {
    const r = await fetch(ORIGIN + path);
    rec[ORIGIN + path] = { status: r.status, contentType: r.headers.get('content-type'), body: await r.text() };
  }
  writeFileSync(REC, JSON.stringify(rec, null, 2));
}
const recording = new Map(Object.entries(JSON.parse(readFileSync(REC))));

// seeded RNG for the page AND its workers. Workers get it by wrapping the Worker constructor
// (classic workers only; module workers would need CDP auto-attach)
function seedRng() {
  let s = 42;
  const next = () => (s = (s * 1664525 + 1013904223) >>> 0);
  Math.random = () => next() / 2 ** 32;
  crypto.getRandomValues = (a) => { for (let i = 0; i < a.length; i++) a[i] = next(); return a; };
  crypto.randomUUID = () => {
    const h = [...Array(32)].map(() => (next() & 15).toString(16));
    h[12] = '4'; h[16] = '89ab'[next() & 3];
    const x = h.join('');
    return `${x.slice(0, 8)}-${x.slice(8, 12)}-${x.slice(12, 16)}-${x.slice(16, 20)}-${x.slice(20)}`;
  };
  if (typeof Worker !== 'undefined') {
    const Real = Worker;
    globalThis.Worker = function (url, opts) {
      const src = `(${seedRng})(); importScripts(${JSON.stringify(new URL(url, location.href).href)});`;
      return new Real(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })), opts);
    };
  }
}

const FIXES = ['network', 'rng', 'vtime', 'frames'];
const T0 = 1700000000; // fake epoch seconds for virtual time
const sha = (x) => createHash('sha256').update(x).digest('hex').slice(0, 12);
const sleep = (ms) => new Promise((ok) => setTimeout(ok, ms));

async function render(browser, on) {
  const ctx = await browser.newContext({ viewport: { width: 400, height: 500 } });
  try {
    if (on.network) {
      await ctx.route('**/*', (route) => {
        const rec = recording.get(route.request().url());
        return rec ? route.fulfill(rec) : route.abort('blockedbyclient'); // off-recording
      });
    }
    if (on.rng) await ctx.addInitScript(seedRng);
    const page = await ctx.newPage();
    const cdp = await ctx.newCDPSession(page);

    // one step = let time move then (with frame control) draw one frame
    // frames get fake timestamps too; without them animations follow the real clock
    let frameTicks = 1000;
    const frame = (extra = {}) => cdp.send('HeadlessExperimental.beginFrame',
      { frameTimeTicks: (frameTicks += 16), interval: 16, ...extra });
    const step = async () => {
      if (on.vtime) {
        const expired = new Promise((ok) => cdp.once('Emulation.virtualTimeBudgetExpired', ok));
        await cdp.send('Emulation.setVirtualTimePolicy', { policy: 'pauseIfNetworkFetchesPending', budget: 16 });
        await expired;
      } else {
        await sleep(16);
      }
      if (on.frames) await frame();
    };

    if (on.vtime) await cdp.send('Emulation.setVirtualTimePolicy', { policy: 'pause', initialVirtualTime: T0 });
    cdp.send('Page.navigate', { url: ORIGIN + '/' }); // replies only after time moves so do not await
    let steps = 0;
    while (!(await page.evaluate(() => window.done === true).catch(() => false))) {
      if (++steps > 300) return { dom: 'TIMEOUT', a11y: 'TIMEOUT', png: 'TIMEOUT', steps };
      await step();
    }

    const dom = sha(await page.content());
    const a11y = sha(await page.locator('body').ariaSnapshot());
    const png = on.frames
      ? sha((await frame({ screenshot: { format: 'png' } })).screenshotData ?? 'no-frame')
      : sha(await page.screenshot());
    return { dom, a11y, png, steps };
  } finally {
    await ctx.close();
  }
}

async function run(name, on) {
  const browser = await chromium.launch({ args: on.frames ? ['--deterministic-mode'] : [] });
  const seen = { dom: new Set(), a11y: new Set(), png: new Set(), steps: new Set() };
  let timeouts = 0;
  for (let i = 0; i < N; i++) {
    const r = await render(browser, on);
    if (r.dom === 'TIMEOUT') timeouts++;
    for (const k in seen) seen[k].add(String(r[k]));
  }
  await browser.close();
  const row = `| ${name.padEnd(14)} | ${[seen.dom, seen.a11y, seen.png, seen.steps].map((s) => String(s.size).padStart(4)).join(' | ')} | ${String(timeouts).padStart(8)} |`;
  console.log(row);
  return seen;
}

console.log(`N = ${N} renders per config. Cells = unique hashes (1 = deterministic).\n`);
console.log('| config         |  dom | a11y |  png | steps | timeouts |');
console.log('|----------------|------|------|------|-------|----------|');
const all = Object.fromEntries(FIXES.map((f) => [f, true]));
const ref = await run('all fixes', all);
if (!process.env.ALL_ONLY) for (const f of FIXES) await run(`no ${f}`, { ...all, [f]: false });
if (!process.env.ALL_ONLY) await run('no fixes', Object.fromEntries(FIXES.map((f) => [f, false])));
// compare these across machines: same hashes = same render
console.log(`\nall-fixes hashes: dom=${[...ref.dom]} a11y=${[...ref.a11y]} png=${[...ref.png]}`);
server.close();
