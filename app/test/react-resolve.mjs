import { stat } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

export async function resolve(specifier, context, next) {
  if (specifier.startsWith('.') && context.parentURL?.includes('/src/') && !/\.[cm]?[jt]s$|\.json$/.test(specifier)) {
    for (const ext of ['.ts', '.js']) {
      const url = new URL(specifier + ext, context.parentURL);
      try { if ((await stat(fileURLToPath(url))).isFile()) return next(url.href, context); } catch { /* Try next extension. */ }
    }
  }
  return next(specifier, context);
}
