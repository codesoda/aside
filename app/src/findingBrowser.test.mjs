import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
import { findings } from '../test/fixtures.mjs';

installDom();
const { mount, unmount, flushSync } = await import('svelte');
const { default: FindingBrowser } = await import('./insights/FindingBrowser.svelte');
let app;
afterEach(() => { if (app) unmount(app); app = null; document.body.innerHTML = ''; });
function render(items, drafts = new Set()) {
  const actions = [];
  app = mount(FindingBrowser, { target: document.body, props: {
    findings: items, loading: false, includeHistorical: true, pending: null, error: null, findingsWithDrafts: drafts,
    onHistorical: (value) => actions.push(['historical', value]),
    onState: (item, disposition, until) => actions.push(['state', item.finding.id, disposition, until]),
    onPromote: (item) => actions.push(['promote', item.finding.id]),
    onOpenDraft: (item) => actions.push(['draft', item.finding.id]),
  } });
  flushSync();
  return actions;
}
function click(label) {
  const button = [...document.querySelectorAll('button')].find((node) => node.textContent.trim() === label);
  assert.ok(button, label);
  button.click(); flushSync();
}
const item = (index, disposition, label = 'current') => ({ ...findings[index], label, disposition,
  userState: { revision: 1, disposition, snoozedUntil: null, promotedDraftId: null }, canPromote: label === 'current' });

test('browse dismissed and snoozed findings, restore and promote, while historical stays read-only', () => {
  const actions = render([item(0, 'untouched'), item(1, 'dismissed_local'), item(2, 'snoozed'), item(0, 'untouched', 'historical')]);
  click('Dismissed');
  assert.equal(document.querySelectorAll('article[data-insight-class]').length, 1);
  click(`Restore ${findings[1].finding.title}`);
  click(`Promote to draft`);
  assert.deepEqual(actions.slice(0, 2), [['state', findings[1].finding.id, 'untouched', undefined], ['promote', findings[1].finding.id]]);
  click('Snoozed');
  assert.equal(document.querySelectorAll('article[data-insight-class]').length, 1);
  click('Historical');
  assert.match(document.body.textContent, /Past snapshots only/);
  assert.equal(document.querySelectorAll('article[data-insight-class]').length, 1);
  assert.equal([...document.querySelectorAll('button')].some((button) => button.textContent.includes('Promote to draft')), false);
});

test('existing promoted draft is opened rather than promoted again', () => {
  const promoted = item(0, 'promoted_to_draft');
  promoted.userState.promotedDraftId = 'draft-1';
  const actions = render([promoted], new Set([promoted.finding.id]));
  click('Open existing draft');
  assert.deepEqual(actions, [['draft', promoted.finding.id]]);
});

test('a discarded draft ID does not hide the promotion action', () => {
  const promoted = item(0, 'promoted_to_draft');
  promoted.userState.promotedDraftId = 'discarded';
  const actions = render([promoted]);
  click('Promote to draft');
  assert.deepEqual(actions, [['promote', promoted.finding.id]]);
});
