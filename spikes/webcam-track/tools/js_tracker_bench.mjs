// Runs the earlier web prototype's real JS tracker (js/puppet/tracker.js there, MediaPipe Tasks Vision 0.10.14,
// VIDEO mode, CPU/WASM delegate) in headless Chromium over a directory of PNG frames, and writes the result in the
// track_face.py JSON shape plus per-call latency: the JS leg of the Rust-vs-JS comparison.
//
//   node tools/js_tracker_bench.mjs FRAMES_DIR OUT.json [--fps 30] [--passes 3] [--vendor DIR] [--proto DIR]
//
// Headless Chromium has no GPU here, so this is the CPU (WASM + XNNPACK) path; the GPU delegate is not measured.
// The vendored MediaPipe files live outside the repo (see the prototype's vendor/ notes); the prototype is served
// from a local checkout of it and /vendor is mapped to --vendor.
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const args = process.argv.slice(2);
const opt = (k, d) => { const i = args.indexOf('--' + k); return i >= 0 ? args[i + 1] : d; };
const [FRAMES, OUT] = args.filter((a, i) => !a.startsWith('--') && !(i > 0 && args[i - 1].startsWith('--')));
if (!FRAMES || !OUT) { console.error('usage: node js_tracker_bench.mjs FRAMES_DIR OUT.json [--fps 30] [--passes 3] [--vendor DIR]'); process.exit(2); }
const FPS = +opt('fps', 30), PASSES = +opt('passes', 3);
const VENDOR = path.resolve(opt('vendor', path.join(os.homedir(), 'work/prototype-world/vendor')));
const PROTO = path.resolve(opt('proto', process.env.OMUZAKI_JS_PROTOTYPE || ''));
const files = fs.readdirSync(FRAMES).filter((f) => f.endsWith('.png')).sort();

const MIME = { '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.html': 'text/html', '.png': 'image/png', '.task': 'application/octet-stream' };
const server = http.createServer((req, res) => {
  const url = decodeURIComponent(req.url.split('?')[0]);
  if (url === '/blank.html') { res.writeHead(200, { 'content-type': 'text/html' }); res.end('<!doctype html><title>bench</title>'); return; }
  const roots = [['/vendor/', VENDOR], ['/frames/', FRAMES]];
  let file = null;
  for (const [prefix, root] of roots) if (url.startsWith(prefix)) file = path.join(root, url.slice(prefix.length));
  if (!file) file = path.join(PROTO, url);
  if (!path.resolve(file).startsWith(path.resolve(VENDOR)) && !path.resolve(file).startsWith(path.resolve(FRAMES)) && !path.resolve(file).startsWith(PROTO)) { res.writeHead(403); res.end(); return; }
  fs.readFile(file, (err, data) => {
    if (err) { res.writeHead(404); res.end(); return; }
    res.writeHead(200, { 'content-type': MIME[path.extname(file)] || 'application/octet-stream' }); res.end(data);
  });
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;

const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  page.on('console', (m) => { if (m.type() === 'error') console.error('[page]', m.text()); });
  await page.goto(`${base}/blank.html`);
  const result = await page.evaluate(async ({ files, fps, passes }) => {
    const { createTracker } = await import('/js/puppet/tracker.js');
    const tracker = await createTracker({ delegate: 'cpu' });
    const frames = [];
    for (const f of files) {
      const blob = await (await fetch(`/frames/${f}`)).blob();
      frames.push(await createImageBitmap(blob, { colorSpaceConversion: 'none', premultiplyAlpha: 'none' }));
    }
    // Warm-up on throwaway timestamps far below the real ones is not allowed (VIDEO mode needs increasing time), so the
    // first pass is the measured/recorded one and its first call includes WASM start-up; latency stats skip the first 5.
    const out = [], ms = [];
    for (let p = 0; p < passes; p++) {
      for (let i = 0; i < frames.length; i++) {
        const t = (p * frames.length + i) / fps;
        const t0 = performance.now();
        const r = tracker.detect(frames[i], t * 1000);
        const dt = performance.now() - t0;
        ms.push(dt);
        if (p === 0) out.push(r ? { t: Math.round(t * 1e4) / 1e4, found: true, bs: r.bs, m: r.m } : { t: Math.round(t * 1e4) / 1e4, found: false });
      }
    }
    return { names: tracker.names, frames: out, ms, delegate: tracker.delegate, layout: tracker.layout, ua: navigator.userAgent };
  }, { files, fps: FPS, passes: PASSES });

  const timed = result.ms.slice(5).sort((a, b) => a - b);
  const q = (p) => timed[Math.min(timed.length - 1, Math.ceil(p * timed.length) - 1)];
  const summary = { n: timed.length, mean: timed.reduce((a, b) => a + b, 0) / timed.length, p50: q(0.5), p95: q(0.95), max: timed[timed.length - 1] };
  const found = result.frames.filter((f) => f.found).length;
  fs.writeFileSync(OUT, JSON.stringify({
    fps: FPS, source: path.resolve(FRAMES), start: 0, names: result.names, frames: result.frames, detect_rate: found / result.frames.length,
    settings: { delegate: result.delegate, tasks_vision: '0.10.14', browser: result.ua, passes: PASSES, matrix_layout: result.layout },
    ms_per_call: summary,
  }));
  console.log(`frames ${result.frames.length} found ${found}  delegate ${result.delegate}  ${PASSES} passes: mean ${summary.mean.toFixed(2)} ms  p50 ${summary.p50.toFixed(2)}  p95 ${summary.p95.toFixed(2)}  wrote ${OUT}`);
} finally {
  await browser.close();
  server.close();
}
