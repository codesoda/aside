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
const { default: FileNavigator } = await vite.ssrLoadModule('/src/workspace/FileNavigator.tsx');
let root;
afterEach(async () => { if (root) await act(async () => root.unmount()); root = null; document.body.innerHTML = ''; });
test.after(async () => vite.close());
const files = ['src/a.ts', 'src/deep/b.rs', '.gitignore', 'README', 'folder/.config.json', 'folder/file.'].map((path, index) => ({ id: String(index), path }));
const states = files.map((file, index) => ({ fileId: file.id, reviewStatus: 'unreviewed', scope: { ignored: index === 1 } }));
async function render(extra = {}) {
  const selected = [], scoped = [], preferences = [];
  root = createRoot(document.body);
  await act(async () => root.render(React.createElement(FileNavigator, { files, states, selectedId: '0', onSelect: (id) => selected.push(id), onScope: (target) => scoped.push(target), onTypePreference: (...args) => preferences.push(args), ...extra })));
  return { selected, scoped, preferences };
}
function button(label) { const node = [...document.querySelectorAll('button')].find((b) => b.getAttribute('aria-label') === label); assert.ok(node, label); return node; }
async function click(label) { await act(async () => button(label).click()); }
async function input(label, value) { const node = document.querySelector(`[aria-label="${label}"]`); assert.ok(node); await act(async () => { Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set.call(node, value); node.dispatchEvent(new window.Event('input', { bubbles: true })); }); }
async function change(node, value) { assert.ok(node); await act(async () => { node.value = value; node.dispatchEvent(new window.Event('change', { bubbles: true })); }); }

test('compact single-row file and keyboard-accessible navigation remains available while scope is pending', async () => {
  const { selected } = await render({ pending: true });
  assert.equal(button('Open src/a.ts').getAttribute('aria-current'), 'true');
  assert.equal(button('Open src/a.ts').closest('li').querySelectorAll('.main').length, 1);
  await click('Open src/a.ts');
  assert.deepEqual(selected, ['0']);
  await click('Actions for file src/a.ts');
  assert.equal(button('Actions for file src/a.ts').getAttribute('aria-expanded'), 'true');
  assert.equal(document.querySelector('.menu button').disabled, true);
});

test('literal/regex path search, invalid regex errors, folder collapse and filter popover', async () => {
  await render();
  await click('File filters');
  await act(async () => [...document.querySelectorAll('.popover label')].find((label) => label.textContent.includes('Show ignored')).querySelector('input').click());
  await click('File filters');
  await input('Search file paths', 'SRC/DEEP');
  assert.ok(button('Collapse folder src/deep'));
  await input('Search file paths', 'a.ts');
  assert.ok(button('Open src/a.ts'));
  await click('File filters');
  assert.equal(button('File filters').getAttribute('aria-expanded'), 'true');
  assert.match(document.querySelector('.popover').textContent, /No extension/);
  assert.match(document.querySelector('.popover').textContent, /Dotfiles/);
  assert.ok([...document.querySelectorAll('.type-row input')].every((node) => node.checked));
  await act(async () => document.querySelector('.regex input').click());
  await input('Search file paths', '[');
  assert.match(document.querySelector('[role=alert]').textContent, /Invalid regex/);
});

test('file include override and folder exclusion send distinct callbacks; type preference is separate from visibility', async () => {
  const { scoped, preferences } = await render();
  await click('Actions for file src/a.ts');
  await act(async () => [...document.querySelectorAll('.menu button')].find((b) => b.textContent === 'Include this review').click());
  assert.deepEqual(scoped, [{ kind: 'file', expression: 'src/a.ts', action: 'include', lifetime: 'review' }]);
  await click('Actions for folder src');
  await act(async () => [...document.querySelectorAll('.menu button')].find((b) => b.textContent === 'Exclude always').click());
  assert.deepEqual(scoped[1], { kind: 'directory', expression: 'src', action: 'exclude', lifetime: 'repository' });
  await click('File filters');
  await change(document.querySelector('select[aria-label="Repository preference for .ts"]'), 'exclude');
  for (const [label, category] of [['No extension', 'no-extension'], ['Dotfiles', 'dotfile']]) {
    const preference = document.querySelector(`select[aria-label="Repository preference for ${label}"]`);
    assert.ok(preference, label);
    await change(preference, 'include');
    assert.equal(preferences.at(-1)[0], category);
  }
  assert.deepEqual(preferences, [['.ts', 'exclude'], ['no-extension', 'include'], ['dotfile', 'include']]);
  assert.ok(preferences.every(([category]) => category !== ''));
  assert.ok(button('Open src/a.ts'));
  const checkbox = [...document.querySelectorAll('.type-row')].find((row) => row.textContent.includes('.ts')).querySelector('input');
  await act(async () => checkbox.click());
  assert.equal(document.querySelector('[aria-label="Open src/a.ts"]'), null);
  await act(async () => checkbox.click());
  assert.ok(button('Open src/a.ts'));
});

test('server preferences, effective rule, retry, Escape and outside pointer close', async () => {
  let retries = 0;
  const ignored = states.map((state, index) => index === 1 ? { ...state, scope: { ignored: true, effectiveRuleId: 'r1' } } : state);
  await render({ states: ignored, rules: [{ id: 'r1', kind: 'directory', expression: 'src/deep', action: 'exclude' }], typePreferences: { '.ts': 'exclude' }, error: 'Scope failed', onRetry: () => retries++ });
  assert.match(document.querySelector('[role=alert]').textContent, /Scope failed/);
  await act(async () => document.querySelector('[role=alert] button').click()); assert.equal(retries, 1);
  await click('File filters');
  assert.equal(document.querySelector('select[aria-label="Repository preference for .ts"]').value, 'exclude');
  const ignoredCheckbox = [...document.querySelectorAll('.popover label')].find((label) => label.textContent.includes('Show ignored')).querySelector('input');
  await act(async () => ignoredCheckbox.click());
  assert.match(button('Open src/deep/b.rs — excluded by directory rule src/deep').title, /directory rule src\/deep/);
  await act(async () => document.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })));
  assert.equal(document.activeElement, button('File filters'));
  assert.equal(document.querySelector('.popover'), null);
  await click('Actions for file src/a.ts');
  await act(async () => document.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })));
  assert.equal(document.activeElement, button('Actions for file src/a.ts'));
  assert.equal(document.querySelector('.menu'), null);
  await click('Actions for file src/a.ts');
  await act(async () => document.body.dispatchEvent(new window.PointerEvent('pointerdown', { bubbles: true })));
  assert.equal(document.querySelector('.menu'), null);
  await click('File filters');
  const view = [...document.querySelectorAll('.popover select')].find((node) => node.closest('label')?.textContent.includes('View'));
  await change(view, 'priority');
  assert.equal(button('Open src/a.ts').querySelector('.name').textContent, 'src/a.ts');
  assert.ok(button('Open src/a.ts').querySelector('svg'));
});
