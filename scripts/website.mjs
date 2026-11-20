import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';

const site = new URL('../web/', import.meta.url);
const files = new Map([
  ['/', ['index.html', 'text/html; charset=utf-8']],
  ['/index.html', ['index.html', 'text/html; charset=utf-8']],
  ['/README.md', ['README.md', 'text/markdown; charset=utf-8']],
  ['/agents.md', ['agents.md', 'text/markdown; charset=utf-8']],
  ['/og-aside.jpg', ['og-aside.jpg', 'image/jpeg']],
  ['/desktop.png', ['desktop.png', 'image/png']],
  ['/mobile.png', ['mobile.png', 'image/png']],
]);
const rawPort = process.env.ASIDE_WEB_PORT ?? '4173';
const port = Number(rawPort);
if (!/^\d+$/.test(rawPort) || !Number.isInteger(port) || port < 1 || port > 65535) {
  console.error(`Invalid ASIDE_WEB_PORT: ${rawPort}`);
  process.exit(1);
}

const server = createServer(async (request, response) => {
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    response.writeHead(405, { Allow: 'GET, HEAD' }).end();
    return;
  }
  let pathname;
  try {
    pathname = new URL(request.url, 'http://localhost').pathname;
  } catch {
    response.writeHead(400).end();
    return;
  }
  const entry = files.get(pathname);
  if (!entry) {
    response.writeHead(404).end();
    return;
  }
  try {
    const [filename, contentType] = entry;
    const content = await readFile(new URL(filename, site));
    response.writeHead(200, {
      'Content-Type': contentType,
      'Content-Length': content.length,
      'X-Content-Type-Options': 'nosniff',
      'Cache-Control': 'no-store',
    });
    response.end(request.method === 'HEAD' ? undefined : content);
  } catch (error) {
    console.error(`Cannot read website file: ${error.message}`);
    response.writeHead(500).end();
  }
});

server.on('error', (error) => {
  console.error(`Cannot start website: ${error.message}`);
  process.exitCode = 1;
});
server.listen(port, '127.0.0.1', () => {
  console.log(`Website: http://127.0.0.1:${port}/`);
});
