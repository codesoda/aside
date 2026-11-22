import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
installDom();
const { mount, unmount, flushSync } = await import('svelte');
const { default: FileNavigator } = await import('./workspace/FileNavigator.svelte');
let app;
afterEach(() => { if (app) unmount(app); app = null; document.body.innerHTML = ''; });
const files = ['src/a.ts', 'src/deep/b.rs', '.gitignore', 'README', 'folder/.config.json', 'folder/file.'].map((path, index) => ({ id: String(index), path }));
const states = files.map((file, index) => ({ fileId: file.id, reviewStatus: 'unreviewed', scope: { ignored: index === 1 } }));
function render(extra = {}) {
  const selected = [], scoped = [], preferences = [];
  app = mount(FileNavigator, { target: document.body, props: { files, states, selectedId: '0', onSelect: (id) => selected.push(id), onScope: (target) => scoped.push(target), onTypePreference: (...args) => preferences.push(args), ...extra } });
  flushSync();
  return { selected, scoped, preferences };
}
function button(label) { const node = [...document.querySelectorAll('button')].find((b) => b.getAttribute('aria-label') === label); assert.ok(node, label); return node; }
function click(label) { button(label).click(); flushSync(); }
function input(label, value) { const node = document.querySelector(`[aria-label="${label}"]`); node.value = value; node.dispatchEvent(new window.Event('input', { bubbles: true })); flushSync(); }

test('compact single-row file and keyboard-accessible navigation remains available while scope is pending', () => {
  const { selected } = render({ pending: true, showIgnored: true });
  assert.equal(button('Open src/a.ts').getAttribute('aria-current'), 'true');
  assert.equal(button('Open src/a.ts').closest('li').querySelectorAll('.main').length, 1);
  click('Open src/a.ts');
  assert.deepEqual(selected, ['0']);
  click('Actions for file src/a.ts');
  assert.equal(button('Actions for file src/a.ts').getAttribute('aria-expanded'), 'true');
  assert.equal(document.querySelector('.menu button').disabled, true);
});

test('literal/regex path search, invalid regex errors, folder collapse and filter popover', () => {
  render();
  click('File filters');
  [...document.querySelectorAll('.popover label')].find((label) => label.textContent.includes('Show ignored')).querySelector('input').click(); flushSync();
  click('File filters');
  input('Search file paths', 'SRC/DEEP');
  assert.ok(button('Collapse folder src/deep'));
  input('Search file paths', 'a.ts');
  assert.ok(button('Open src/a.ts'));
  click('File filters');
  assert.equal(button('File filters').getAttribute('aria-expanded'), 'true');
  assert.match(document.querySelector('.popover').textContent, /No extension/);
  assert.match(document.querySelector('.popover').textContent, /Dotfiles/);
  assert.ok([...document.querySelectorAll('.type-row input')].every((node) => node.checked));
  const regex = document.querySelector('.regex input'); regex.click(); flushSync();
  input('Search file paths', '[');
  assert.match(document.querySelector('[role=alert]').textContent, /Invalid regex/);
});

test('file include override and folder exclusion send distinct callbacks; type preference is separate from visibility', () => {
  const { scoped, preferences } = render();
  click('Actions for file src/a.ts');
  [...document.querySelectorAll('.menu button')].find((b) => b.textContent === 'Include this review').click(); flushSync();
  assert.deepEqual(scoped, [{ kind: 'file', expression: 'src/a.ts', action: 'include', lifetime: 'review' }]);
  click('Actions for folder src');
  [...document.querySelectorAll('.menu button')].find((b) => b.textContent === 'Exclude always').click(); flushSync();
  assert.deepEqual(scoped[1], { kind: 'directory', expression: 'src', action: 'exclude', lifetime: 'repository' });
  click('File filters');
  const select = document.querySelector('select[aria-label="Repository preference for .ts"]'); select.value = 'exclude'; select.dispatchEvent(new window.Event('change', { bubbles: true })); flushSync();
  for (const [label, category] of [['No extension', 'no-extension'], ['Dotfiles', 'dotfile']]) {
    const preference = document.querySelector(`select[aria-label="Repository preference for ${label}"]`);
    assert.ok(preference, label);
    preference.value = 'include'; preference.dispatchEvent(new window.Event('change', { bubbles: true })); flushSync();
    assert.equal(preferences.at(-1)[0], category);
  }
  assert.deepEqual(preferences, [['.ts', 'exclude'], ['no-extension', 'include'], ['dotfile', 'include']]);
  assert.ok(preferences.every(([category]) => category !== ''));
  assert.ok(button('Open src/a.ts'));
  const checkbox = [...document.querySelectorAll('.type-row')].find((row) => row.textContent.includes('.ts')).querySelector('input');
  checkbox.click(); flushSync();
  assert.equal(document.querySelector('[aria-label="Open src/a.ts"]'), null);
  checkbox.click(); flushSync();
  assert.ok(button('Open src/a.ts'));
});

test('server preferences, effective rule, retry, Escape and outside pointer close', () => {
  let retries = 0;
  const ignored = states.map((state, index) => index === 1 ? { ...state, scope: { ignored: true, effectiveRuleId: 'r1' } } : state);
  render({ states: ignored, rules: [{ id: 'r1', kind: 'directory', expression: 'src/deep', action: 'exclude' }], typePreferences: { '.ts': 'exclude' }, error: 'Scope failed', onRetry: () => retries++ });
  assert.match(document.querySelector('[role=alert]').textContent, /Scope failed/);
  document.querySelector('[role=alert] button').click(); assert.equal(retries, 1);
  click('File filters');
  assert.equal(document.querySelector('select[aria-label="Repository preference for .ts"]').value, 'exclude');
  const ignoredCheckbox = [...document.querySelectorAll('.popover label')].find((label) => label.textContent.includes('Show ignored')).querySelector('input');
  ignoredCheckbox.click(); flushSync();
  assert.match(button('Open src/deep/b.rs — excluded by directory rule src/deep').title, /directory rule src\/deep/);
  document.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); flushSync();
  assert.equal(document.activeElement, button('File filters'));
  assert.equal(document.querySelector('.popover'), null);
  click('Actions for file src/a.ts');
  document.body.dispatchEvent(new window.PointerEvent('pointerdown', { bubbles: true })); flushSync();
  assert.equal(document.querySelector('.menu'), null);
  click('File filters');
  const view = [...document.querySelectorAll('.popover select')].find((node) => node.closest('label')?.textContent.includes('View'));
  view.value = 'priority'; view.dispatchEvent(new window.Event('change', { bubbles: true })); flushSync();
  assert.equal(button('Open src/a.ts').querySelector('.name').textContent, 'src/a.ts');
  assert.ok(button('Open src/a.ts').querySelector('svg'));
});
