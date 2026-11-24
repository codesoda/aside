import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';

// Run the executable out of tree with no Node, Vite, or built assets on disk.
const root = process.env.ASIDE_DATA_DIR;
assert(root, 'Set ASIDE_DATA_DIR to a private directory outside the checkout');
const binary = resolve(process.argv[2] ?? 'target/release/aside');
const expected = await readFile(new URL('../app/dist/index.html', import.meta.url), 'utf8');
const script = expected.match(/<script\b[^>]*\bsrc="([^"\s]+\.js)"/)?.[1];
assert(script?.startsWith('/assets/'), 'Build the frontend before building the executable');
const dir = await mkdtemp(join(root, 'embedded-ui-'));
let child;
try {
  const bin = join(dir, 'bin');
  await mkdir(bin);
  const executable = join(dir, 'aside');
  await copyFile(binary, executable);
  await chmod(executable, 0o700);
  child = spawn(executable, ['demo', '--no-open'], {
    cwd: dir,
    env: { PATH: bin, HOME: dir, ASIDE_CONFIG_DIR: join(dir, 'config'), ASIDE_DATA_DIR: root },
    stdio: ['ignore', 'pipe', 'ignore'],
  });
  const output = createInterface({ input: child.stdout });
  const started = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Offline demo did not start within 30 seconds')), 30_000);
    child.once('error', (error) => { clearTimeout(timer); reject(error); });
    child.once('exit', (code) => { clearTimeout(timer); reject(new Error(`Offline demo exited: ${code}`)); });
    output.on('line', (line) => {
      let value;
      try { value = JSON.parse(line); } catch { return; }
      if (value?.event === 'session.started') { clearTimeout(timer); resolve(value); }
    });
  });
  const get = (path) => fetch(new URL(path, started.url), { signal: AbortSignal.timeout(5_000) });
  const health = await get('/api/v1/health');
  assert.equal(health.status, 200);
  assert.equal((await health.json()).sessionId, started.sessionId);
  assert.equal((await get('/api/v1/session')).status, 401);
  const page = await get('/');
  assert.equal(page.status, 200);
  assert.ok((await page.text()) === expected, 'The executable does not embed the current frontend build');
  const asset = await get(script);
  assert.equal(asset.status, 200);
  assert.equal(await asset.text(), await readFile(new URL(`../app/dist${script}`, import.meta.url), 'utf8'));
  console.log('Embedded review UI serves the production build without Node or Vite.');
} finally {
  if (child?.pid && child.exitCode === null && child.signalCode === null) {
    const closed = once(child, 'close');
    child.kill('SIGKILL');
    await closed;
  }
  await rm(dir, { recursive: true, force: true });
}
