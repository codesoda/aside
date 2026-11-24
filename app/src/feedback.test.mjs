import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { createRequire } from 'node:module';
import { installDom } from '../test/dom.mjs';
installDom();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const require = createRequire(import.meta.url);
const React = require('react');
const { act } = React;
const { createRoot } = require('react-dom/client');
const { createServer } = await import('vite');
const vite = await createServer({ server: { middlewareMode: true, hmr: false }, appType: 'custom' });
const { default: Feedback } = await vite.ssrLoadModule('/src/workspace/Feedback.tsx');
let root;
afterEach(async () => { await act(async () => root.unmount()); document.body.innerHTML = ''; });
test.after(async () => vite.close());
const coverage = (complete) => ({ coverage: { complete, sources: [{ sourceId: 'github', state: 'available', completeness: complete ? 'complete' : 'partial', itemKind: 'thread', items: { state: 'known', value: 1 }, runId: null, limitations: complete ? [] : ['page unavailable'] }] } });
async function render(props) { root = createRoot(document.body); await act(async () => root.render(React.createElement(Feedback, { threads: [], records: [], threadPage: null, recordPage: null, loading: false, errors: [], ignoredPaths: new Set(), availablePaths: new Set(), headSha: 'head', onOpenFile: () => {}, ...props }))); }
test('partial native coverage and errors do not claim an all-clear', async () => {
  await render({ threadPage: coverage(false), recordPage: coverage(false), errors: ['page two unavailable'] });
  assert.match(document.body.textContent, /Search results are incomplete/);
  assert.match(document.body.textContent, /coverage incomplete/);
  assert.match(document.body.textContent, /empty results are not all clear/);
  assert.doesNotMatch(document.body.textContent, /No imported review threads/);
});
test('only complete empty native sources report no imported feedback', async () => {
  await render({ threadPage: coverage(true), recordPage: coverage(true) });
  assert.match(document.body.textContent, /No imported review threads \(complete source\)/);
  assert.match(document.body.textContent, /No other imported feedback \(complete sources\)/);
});
