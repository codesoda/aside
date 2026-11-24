import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { createRequire } from 'node:module';
import { installDom } from '../test/dom.mjs';
import { findings } from '../test/fixtures.mjs';
installDom();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const require = createRequire(import.meta.url);
const React = require('react');
const { act } = React;
const { createRoot } = require('react-dom/client');
const { createServer } = await import('vite');
const vite = await createServer({ server: { middlewareMode: true, hmr: false }, appType: 'custom' });
const { default: FindingBrowser } = await vite.ssrLoadModule('/src/insights/FindingBrowser.tsx');
test.after(async () => vite.close());
let root;
afterEach(async () => { if (root) await act(async () => root.unmount()); root = null; document.body.innerHTML = ''; });
async function render(items, drafts = new Set()) {
  const actions = [];
  root = createRoot(document.body);
  await act(async () => root.render(React.createElement(FindingBrowser, {
    findings: items.filter(item => item.label === 'current'), loading: false,
    historicalFindings: items.filter(item => item.label === 'historical'), historicalLoading: false, historicalLoadError: null,
    pending: null, error: null, findingsWithDrafts: drafts,
    onHistorical: () => actions.push(['historical']), onHistoricalRetry: () => actions.push(['retry-history']),
    onState: (item, disposition, until) => actions.push(['state', item.finding.id, disposition, until]),
    onPromote: item => actions.push(['promote', item.finding.id]),
    onOpenDraft: item => actions.push(['draft', item.finding.id]),
  })));
  return actions;
}
async function click(label) {
  const button = [...document.querySelectorAll('button')].find(node => node.textContent.trim() === label);
  assert.ok(button, label);
  await act(async () => button.click());
}
const item = (index, disposition, label = 'current') => ({ ...findings[index], label, disposition,
  userState: { revision: 1, disposition, snoozedUntil: null, promotedDraftId: null }, canPromote: label === 'current' });

test('browse dismissed and snoozed findings, restore and promote, while historical stays read-only', async () => {
  const actions = await render([item(0, 'untouched'), item(1, 'dismissed_local'), item(2, 'snoozed'), item(0, 'untouched', 'historical')]);
  await click('Dismissed');
  assert.equal(document.querySelectorAll('article[data-insight-class]').length, 1);
  await click(`Restore ${findings[1].finding.title}`);
  await click('Promote to draft');
  assert.deepEqual(actions.slice(0, 2), [['state', findings[1].finding.id, 'untouched', undefined], ['promote', findings[1].finding.id]]);
  await click('Snoozed');
  assert.equal(document.querySelectorAll('article[data-insight-class]').length, 1);
  await click('Historical');
  assert.match(document.body.textContent, /Past snapshots only/);
  assert.equal(document.querySelectorAll('article[data-insight-class]').length, 1);
  assert.equal([...document.querySelectorAll('button')].some(button => button.textContent.includes('Promote to draft')), false);
  assert.deepEqual(actions.at(-1), ['historical']);
});

test('existing promoted draft is opened rather than promoted again', async () => {
  const promoted = item(0, 'promoted_to_draft');
  promoted.userState.promotedDraftId = 'draft-1';
  const actions = await render([promoted], new Set([promoted.finding.id]));
  await click('Open existing draft');
  assert.deepEqual(actions, [['draft', promoted.finding.id]]);
});

test('a discarded draft ID does not hide the promotion action', async () => {
  const promoted = item(0, 'promoted_to_draft');
  promoted.userState.promotedDraftId = 'discarded';
  const actions = await render([promoted]);
  await click('Promote to draft');
  assert.deepEqual(actions, [['promote', promoted.finding.id]]);
});
