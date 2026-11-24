import assert from 'node:assert/strict';
import { after, afterEach, test } from 'node:test';
import { createRequire } from 'node:module';
import { createServer } from 'vite';
import { installDom } from '../../test/dom.mjs';

installDom();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const require = createRequire(import.meta.url);
const { act, createElement } = require('react');
const { createRoot } = require('react-dom/client');
const vite = await createServer({ server: { middlewareMode: true, hmr: false }, appType: 'custom' });
const { DiffView } = await vite.ssrLoadModule('/src/diff/DiffView.tsx');
after(() => vite.close());
let root;
afterEach(async () => {
  await act(async () => root?.unmount());
  document.body.replaceChildren();
});

const lines = [
  { id: 'old', kind: 'deletion', oldLine: 10, newLine: null, text: 'before <b> and old' },
  { id: 'new', kind: 'addition', oldLine: null, newLine: 12, text: 'after <b> and new' },
  { id: 'ctx', kind: 'context', oldLine: 11, newLine: 13, text: 'context' },
];
const diff = { path: 'example.txt', patchComplete: true, hunks: [{ id: 'h', oldStart: 10, newStart: 12, oldCount: 2, newCount: 2, lines }] };
async function mount(props) {
  root = createRoot(document.body);
  await act(async () => root.render(createElement(DiffView, { diff, ...props })));
}

test('unified rows expose exact selectable source text and separate highlights', async () => {
  await mount({ readOnly: true });
  const old = document.querySelector('[data-side=LEFT]');
  const next = document.querySelector('[data-side=RIGHT]');
  assert.equal(old.textContent, lines[0].text);
  assert.equal(next.textContent, lines[1].text);
  assert.deepEqual([...old.querySelectorAll('mark')].map((part) => part.textContent), ['before', 'old']);
  assert.equal(document.querySelector('b'), null);
  assert.equal(old.previousElementSibling.textContent, '-');
  assert.equal(next.previousElementSibling.textContent, '+');
});

test('current LEFT range starts at its old row and spans LEFT context without placing stale markers', async () => {
  const range = { id: 'range', side: 'LEFT', line: 11, startLine: 10, anchorState: 'current' };
  const stale = { id: 'stale', side: 'RIGHT', line: 12, anchorState: 'orphaned' };
  await mount({ threads: [range, stale], selectedThreadId: range.id });
  assert.equal(document.querySelector('[data-thread-anchor=range]').querySelector('[data-code-line]').dataset.codeLine, '10');
  assert.equal(document.querySelector('[data-thread-anchor=stale]'), null);
  assert.deepEqual([...document.querySelectorAll('.diff-range .diff-code')].map((row) => row.textContent), [lines[0].text, lines[2].text]);
});

test('copying across highlighted rows writes only the selected source text', async () => {
  await mount({ onExplain: () => {} });
  const [old, next] = document.querySelectorAll('.diff-code');
  const selection = window.getSelection();
  const range = document.createRange();
  range.setStart(old.querySelector('mark').firstChild, 2);
  range.setEnd(next.querySelectorAll('mark')[1].firstChild, 2);
  selection.addRange(range);
  const copied = new Map();
  const event = new window.Event('copy', { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'clipboardData', { value: { setData: (type, value) => copied.set(type, value) } });
  old.dispatchEvent(event);
  assert.equal(event.defaultPrevented, true);
  assert.equal(copied.get('text/plain'), 'fore <b> and old\nafter <b> and ne');
  assert.equal(selection.isCollapsed, false);
});

test('a partial diff cannot start an inline comment', async () => {
  await mount({ diff: { ...diff, patchComplete: false }, onSelectLine: () => assert.fail('cannot draft on partial diff') });
  assert.equal(document.querySelector('.diff-draft'), null);
});
