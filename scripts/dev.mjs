#!/usr/bin/env node
// Start the CLI first so Vite can proxy to its actual OS-assigned port.
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { existsSync } from 'node:fs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const viteEntry = join(root, 'app', 'node_modules', 'vite', 'bin', 'vite.js');
if (!existsSync(viteEntry)) {
  console.error('Install frontend dependencies first: pnpm -C app install --frozen-lockfile');
  process.exit(2);
}
const args = process.argv.slice(2);
if (args.length > 1 || (args[0] && args[0] !== 'demo' && !/^https:\/\/github\.com\/[^/]+\/[^/]+\/pull\/[1-9]\d*$/.test(args[0]))) {
  console.error('Usage: node scripts/dev.mjs [demo | https://github.com/OWNER/REPO/pull/NUMBER]');
  process.exit(2);
}
const port = Number(process.env.ASIDE_DEV_PORT || 5173);
if (!Number.isInteger(port) || port < 1 || port > 65535) {
  console.error('ASIDE_DEV_PORT must be a TCP port from 1 to 65535');
  process.exit(2);
}
const frontend = `http://127.0.0.1:${port}/`;
const children = new Set();
let stopping = false;
function stop(signal = 'SIGTERM') {
  if (stopping) return;
  stopping = true;
  for (const child of children) {
    if (child.exitCode === null && child.signalCode === null) child.kill(signal);
  }
}
process.on('SIGINT', () => { process.exitCode = 130; stop('SIGINT'); });
process.on('SIGTERM', () => { process.exitCode = 143; stop(); });

function start(command, commandArgs, options = {}) {
  const child = spawn(command, commandArgs, { cwd: root, ...options });
  children.add(child);
  child.once('close', () => children.delete(child));
  return child;
}

function lines(stream, handle) {
  stream.setEncoding('utf8');
  let pending = '';
  stream.on('data', (chunk) => {
    pending += chunk;
    let end;
    while ((end = pending.indexOf('\n')) !== -1) {
      handle(pending.slice(0, end).replace(/\r$/, ''));
      pending = pending.slice(end + 1);
    }
  });
  stream.on('end', () => { if (pending) handle(pending); });
}

async function build() {
  const cargo = start('cargo', ['build', '--locked'], { stdio: 'inherit' });
  const code = await new Promise((resolve, reject) => {
    cargo.once('error', reject);
    cargo.once('exit', (status) => resolve(status));
  });
  if (code !== 0 || stopping) throw new Error(`cargo build failed (${code ?? 'interrupted'})`);
}

async function run() {
  await build();
  const binary = join(root, 'target', 'debug', process.platform === 'win32' ? 'aside.exe' : 'aside');
  const backend = start(binary, [args[0] || 'demo', '--no-open'], {
    env: { ...process.env, ASIDE_DEV_ORIGIN: frontend.slice(0, -1) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let started, login;
  const backendReady = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('CLI did not report session.started and login URL within 120 seconds')), 120_000);
    function ready() {
      if (started && login) { clearTimeout(timer); resolve({ started, login }); }
    }
    lines(backend.stdout, (line) => {
      console.log(line); // JSON events remain on stdout for host agents.
      try {
        const event = JSON.parse(line);
        if (event.event === 'session.started') started = event;
      } catch { /* A diagnostic is not a session event. */ }
      ready();
    });
    lines(backend.stderr, (line) => {
      const prefix = 'Open this private login URL locally: ';
      if (line.startsWith(prefix)) login = line.slice(prefix.length);
      else console.error(line);
      ready();
    });
    backend.once('error', (error) => { clearTimeout(timer); reject(error); });
    backend.once('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`CLI exited before startup (${code})`));
    });
  });
  let session;
  try { session = await backendReady; }
  catch (error) { stop(); throw error; }
  if (stopping) return;

  const vite = start(process.execPath,
    [viteEntry, '--host', '127.0.0.1', '--port', String(port), '--strictPort'],
    { cwd: join(root, 'app'), env: { ...process.env, ASIDE_URL: session.started.url, ASIDE_DEV_PORT: String(port) }, stdio: ['inherit', 'pipe', 'pipe'] });
  const viteReady = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Vite did not become ready within 30 seconds')), 30_000);
    lines(vite.stdout, (line) => {
      console.error(line);
      if (line.includes('Local:') && line.includes(frontend)) { clearTimeout(timer); resolve(); }
    });
    lines(vite.stderr, (line) => console.error(line));
    vite.once('error', (error) => { clearTimeout(timer); reject(error); });
    vite.once('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`Vite exited before startup (${code})`));
    });
  });
  try {
    await Promise.race([
      viteReady,
      new Promise((_, reject) => backend.once('exit', (code) => reject(new Error(`CLI exited while Vite started (${code})`)))),
    ]);
  } catch (error) { stop(); throw error; }
  const loginUrl = new URL(session.login);
  const devLogin = new URL(frontend);
  devLogin.pathname = loginUrl.pathname;
  devLogin.search = loginUrl.search;
  console.log(JSON.stringify({ event: 'frontend.ready', sessionId: session.started.sessionId, url: frontend, apiBase: `${frontend}api/v1` }));
  console.error(`\nOpen the frontend at ${devLogin} (private link). Press Ctrl-C to stop both servers.\n`);
  await Promise.race([
    new Promise((resolve) => backend.once('exit', resolve)),
    new Promise((resolve) => vite.once('exit', resolve)),
  ]);
  if (!stopping) { process.exitCode = 1; stop(); }
}

run().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
  stop();
});
