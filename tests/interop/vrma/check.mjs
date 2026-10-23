// The VRMA conformance check (#1277): plays the fixtures `vrma_interop` wrote in three-vrm in headless Chromium and
// compares every sampled bone rotation, the hips position (after the player's own scaling to the target model),
// every expression weight and the gaze with Omuzaki's own reading. Exits non-zero on any mismatch or loader warning,
// naming the worst value of each kind.
//
//   node check.mjs FIXTURES_DIR
//
// `OMUZAKI_CHROMIUM` points at a Chromium to use; without it, the one in `PLAYWRIGHT_BROWSERS_PATH`, else Playwright's.

import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const TOLERANCE = { rotation: 1e-4, hips: 1e-4, weight: 1e-4, gaze: 1e-4 };

const here = path.dirname(fileURLToPath(import.meta.url));
const fixtures = path.resolve(process.argv[2] ?? path.join(here, 'fixtures'));
const expected = JSON.parse(fs.readFileSync(path.join(fixtures, 'expected.json'), 'utf8'));

const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.json': 'application/json' };

// Serves the page and its modules from here, the fixtures under /fixtures/, and nothing outside the two.
function serve() {
  const server = http.createServer((req, res) => {
    const url = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
    const [root, rel] = url.startsWith('/fixtures/') ? [fixtures, url.slice('/fixtures/'.length)] : [here, url.slice(1)];
    const file = path.resolve(root, rel);
    if (!file.startsWith(root + path.sep) || !fs.existsSync(file) || !fs.statSync(file).isFile()) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { 'content-type': TYPES[path.extname(file)] ?? 'application/octet-stream' });
    fs.createReadStream(file).pipe(res);
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

function chromiumPath() {
  if (process.env.OMUZAKI_CHROMIUM) return process.env.OMUZAKI_CHROMIUM;
  const browsers = process.env.PLAYWRIGHT_BROWSERS_PATH;
  if (!browsers || !fs.existsSync(browsers)) return undefined;
  const builds = fs.readdirSync(browsers).filter((d) => /^chromium-\d+$/.test(d)).sort().reverse();
  for (const build of builds) {
    const exe = path.join(browsers, build, 'chrome-linux', 'chrome');
    if (fs.existsSync(exe)) return exe;
  }
  return undefined;
}

// The angle between two unit quaternions [x, y, z, w], precise for small angles.
function angle(a, b) {
  const [ax, ay, az, aw] = a;
  const [bx, by, bz, bw] = b;
  // conj(a) * b
  const w = aw * bw + ax * bx + ay * by + az * bz;
  const x = aw * bx - ax * bw - ay * bz + az * by;
  const y = aw * by + ax * bz - ay * bw - az * bx;
  const z = aw * bz - ax * by + ay * bx - az * bw;
  return 2 * Math.atan2(Math.hypot(x, y, z), Math.abs(w));
}

const worst = {};
const failures = [];
function record(kind, error, where) {
  if (!(kind in worst) || error > worst[kind].error) worst[kind] = { error, where };
  if (!(error <= TOLERANCE[kind])) failures.push(`${kind} ${where}: off by ${error.toExponential(3)}`);
}

const server = await serve();
const browser = await chromium.launch({ executablePath: chromiumPath() });
let played;
try {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(`http://127.0.0.1:${server.address().port}/page.html`);
  await page.waitForFunction(() => window.ready === true, null, { timeout: 30_000 });
  played = await page.evaluate((manifest) => window.play(manifest), expected);
  if (errors.length) failures.push(...errors.map((e) => `page error: ${e}`));
} finally {
  await browser.close();
  server.close();
}

failures.push(...played.warnings.map((w) => `loader warning: ${w}`));
for (const [model, facts] of Object.entries(expected.models)) {
  const got = played.models[model];
  if (!got) {
    failures.push(`${model}: not played`);
    continue;
  }
  record('hips', Math.abs(got.player_rest_hips_y - facts.hips_height), `${model} rest hips height`);
  for (const animation of expected.animations) {
    const run = got.animations[animation.file];
    if (!run) {
      failures.push(`${model} ${animation.file}: not played`);
      continue;
    }
    record('hips', Math.abs(run.rest_hips_y - animation.rest_hips_y), `${animation.file} rest hips height as read`);
    const scale = facts.hips_height / animation.rest_hips_y;
    animation.times.forEach((t, i) => {
      const sample = run.samples[i];
      const at = `${model} ${animation.file} t=${t}`;
      for (const [bone, values] of Object.entries(animation.bones)) {
        if (sample.bones[bone] == null) continue; // the model has no such bone
        record('rotation', angle(values[i], sample.bones[bone]), `${at} ${bone}`);
      }
      if (animation.hips) {
        const want = animation.hips[i].map((v) => v * scale);
        record('hips', Math.max(...want.map((v, c) => Math.abs(v - sample.hips[c]))), `${at} hips`);
      }
      for (const [name, values] of Object.entries(animation.expressions)) {
        if (sample.expressions[name] == null) {
          failures.push(`${at}: the model has no expression ${name}`);
          continue;
        }
        record('weight', Math.abs(values[i] - sample.expressions[name]), `${at} ${name}`);
      }
      if (animation.look_at) {
        if (!sample.look_at) {
          failures.push(`${at}: the model has no look-at`);
          return;
        }
        const want = animation.look_at[i];
        record('gaze', angle(want.quaternion, sample.look_at.quaternion), `${at} look-at quaternion`);
        const rad = Math.PI / 180;
        record('gaze', Math.abs(want.yaw - sample.look_at.yaw * rad), `${at} yaw`);
        record('gaze', Math.abs(want.pitch - sample.look_at.pitch * rad), `${at} pitch`);
      }
    });
  }
}

for (const [kind, { error, where }] of Object.entries(worst)) {
  console.log(`${kind.padEnd(8)} worst ${error.toExponential(3)} (tolerance ${TOLERANCE[kind]}) at ${where}`);
}
if (failures.length) {
  console.error(`${failures.length} mismatch(es):`);
  for (const f of failures.slice(0, 40)) console.error(`  ${f}`);
  process.exit(1);
}
console.log(`ok: ${expected.animations.length} animations on ${Object.keys(expected.models).length} models`);
