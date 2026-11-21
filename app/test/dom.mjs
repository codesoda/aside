// Minimal browser globals for mounting Svelte components under node:test.
import { JSDOM } from 'jsdom';

export function installDom() {
  const dom = new JSDOM('<!doctype html><html><body></body></html>', { url: 'http://127.0.0.1/', pretendToBeVisual: true });
  const { window } = dom;
  for (const key of Object.getOwnPropertyNames(window)) {
    if (key in globalThis) continue;
    try {
      globalThis[key] = window[key];
    } catch { /* read-only global */ }
  }
  globalThis.window = window;
  globalThis.document = window.document;
  Object.defineProperty(globalThis, 'navigator', { value: window.navigator, configurable: true });
  globalThis.Element.prototype.scrollIntoView ??= () => {};
  globalThis.HTMLElement.prototype.scrollIntoView ??= () => {};
  return dom;
}
