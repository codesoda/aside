import { readFile, stat } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { compile } from 'svelte/compiler';

async function exists(url) {
  try {
    return (await stat(fileURLToPath(url))).isFile();
  } catch {
    return false;
  }
}

export async function resolve(specifier, context, next) {
  const parent = context.parentURL ?? '';
  if (specifier.startsWith('.') && parent.includes('/src/') && !/\.[cm]?[jt]s$|\.svelte$|\.json$/.test(specifier)) {
    for (const ext of ['.ts', '.js']) {
      const url = new URL(specifier + ext, parent);
      if (await exists(url)) return next(url.href, context);
    }
  }
  return next(specifier, context);
}

export async function load(url, context, next) {
  if (url.endsWith('.svelte')) {
    const filename = fileURLToPath(url);
    const source = await readFile(filename, 'utf8');
    const { js } = compile(source, { filename, generate: 'client', dev: false });
    return { format: 'module', source: js.code, shortCircuit: true };
  }
  return next(url, context);
}
