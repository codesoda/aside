import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
import { findings, queueView, sessionView } from '../test/fixtures.mjs';

installDom();
const { mount, unmount, flushSync } = await import('svelte');
const { default: ReviewPrep } = await import('./workspace/ReviewPrep.svelte');
let app;
afterEach(() => { if (app) unmount(app); app = null; document.body.innerHTML = ''; });

function render(extra = {}) {
  const opened = [];
  const props = {
    session: sessionView(), queue: queueView(), insightsError: null,
    onOpenFile: (...args) => opened.push(args), onStart: () => {}, ...extra,
  };
  app = mount(ReviewPrep, { target: document.body, props });
  flushSync();
  return opened;
}
const text = () => document.body.textContent.replace(/\s+/g, ' ');
const click = (label) => {
  const button = [...document.querySelectorAll('button')].find((node) => node.getAttribute('aria-label') === label);
  assert.ok(button, `button ${label}`);
  button.click();
  flushSync();
};
const entry = (id, disposition = 'untouched', attention = 'high', current = true) => ({ id, current, attention, disposition, sourceKind: 'aside_static', providerId: 'aside.diff-facts', severity: { normalized: 'unknown', original: null }, validation: 'not_required' });

test('file header and cited reasons stay visible before findings load', () => {
  const opened = render({ threads: sessionView().threads });
  assert.match(text(), /Unresolved GitHub thread\s*— thread by bob on line 2/);
  assert.match(text(), /finding fnd-measured · provider aside.diff-facts · rule auth_path/);
  assert.match(text(), /Loading findings/);
  click('Review suggested file src/auth.rs');
  assert.deepEqual(opened, [['f-auth']]);
});

test('only current active attention entries display in server order and deep-link', () => {
  const queue = queueView();
  const first = queue.queue.items[0];
  const extras = Array.from({ length: 3 }, (_, i) => ({ ...findings[0], finding: { ...findings[0].finding, id: `extra-${i}`, title: `Extra ${i}` } }));
  first.findings = [entry('fnd-hint'), entry('fnd-measured'), ...extras.map((f) => entry(f.finding.id)), entry('fnd-external', 'dismissed_local'), entry('missing'), entry('historical', 'untouched', 'high', false)];
  const opened = render({ queue, findings: [...findings, ...extras] });
  assert.match(text(), /Consider checking token reuse/);
  assert.match(text(), /Suggestion · attention hint/);
  assert.match(text(), /line 2 of src\/auth.rs · aside.diff-facts/);
  assert.match(text(), /Path matches the auth rule/);
  assert.match(text(), /\+2 more/);
  assert.doesNotMatch(text(), /CodeRabbit said tokens may leak/);
  assert.doesNotMatch(text(), /Extra 1/);
  click('Review finding Consider checking token reuse in src/auth.rs');
  assert.deepEqual(opened, [['f-auth', 'fnd-hint']]);
});

test('empty and loading messages do not claim complete coverage prematurely', () => {
  render({ findings: [], session: sessionView({ overall: 'incomplete' }) });
  assert.match(text(), /not an all-clear/);
});
