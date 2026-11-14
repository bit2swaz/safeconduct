// Runs in every page and worker before its own scripts. The runtime appends (SEED, T0, START).
// Arguments, not globals: a page that declares its own SEED must not break.
((SEED, T0, START) => {
  // The agent's own extension (Stagehand v4 runs in one) is not the site: leave it alone.
  // Checked here, not at attach: Stagehand opens tabs on its extension page, then navigates.
  // Not via location: reading it in a paused worker crashes the renderer (Chrome 156).
  if (globalThis.chrome?.runtime?.id) return;
  // Seeded RNG: Math.random, crypto.getRandomValues and crypto.randomUUID share one stream.
  let s = SEED >>> 0;
  const next = () => (s = (Math.imul(s, 1664525) + 1013904223) >>> 0);
  Math.random = () => next() / 2 ** 32;
  const c = globalThis.crypto;
  if (c) {
    c.getRandomValues = (a) => {
      const b = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
      for (let i = 0; i < b.length; i++) b[i] = next() >>> 24;
      return a;
    };
    c.randomUUID = () => {
      const h = [...Array(32)].map(() => (next() >>> 28).toString(16));
      h[12] = '4';
      h[16] = '89ab'[next() >>> 30];
      const x = h.join('');
      return `${x.slice(0, 8)}-${x.slice(8, 12)}-${x.slice(12, 16)}-${x.slice(16, 20)}-${x.slice(20)}`;
    };
  }
  // Clock: starts at T0 when the runtime started, then moves with real time.
  // ponytail: wall-clock speed, so ms values still differ run to run; timers are not virtual.
  // Exact mode (B1b) needs CDP virtual time with a budget per agent step.
  const RealDate = Date;
  const now = () => T0 + (RealDate.now() - START);
  const FakeDate = function Date(...a) {
    if (!new.target) return new RealDate(now()).toString();
    return a.length ? new RealDate(...a) : new RealDate(now());
  };
  FakeDate.prototype = RealDate.prototype;
  FakeDate.now = now;
  FakeDate.parse = RealDate.parse;
  FakeDate.UTC = RealDate.UTC;
  globalThis.Date = FakeDate;
})