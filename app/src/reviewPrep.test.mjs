import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { createRequire } from 'node:module';
import { installDom } from '../test/dom.mjs';
import { findings, queueView, sessionView } from '../test/fixtures.mjs';

installDom();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const require = createRequire(import.meta.url);
const React = require('react');
const { act } = React;
const { createRoot } = require('react-dom/client');
const { createServer } = await import('vite');
const vite = await createServer({ server: { middlewareMode: true, hmr: false }, appType: 'custom' });
const { default: ReviewPrep } = await vite.ssrLoadModule('/src/workspace/ReviewPrep.tsx');
let root;
afterEach(async () => { if (root) await act(async () => root.unmount()); root = null; document.body.innerHTML = ''; });
test.after(async () => vite.close());

async function render(extra = {}) {
  const opened = [];
  const props = {
    session: sessionView(), queue: queueView(), insightsError: null,
    onOpenFile: (...args) => opened.push(args), onStart: () => {}, ...extra,
  };
  root = createRoot(document.body);
  await act(async () => root.render(React.createElement(ReviewPrep, props)));
  return opened;
}
const text = () => document.body.textContent.replace(/\s+/g, ' ');
const click = async (label) => {
  const button = [...document.querySelectorAll('button')].find((node) => node.getAttribute('aria-label') === label);
  assert.ok(button, `button ${label}`);
  await act(async () => button.click());
};
const entry = (id, disposition = 'untouched', attention = 'high', current = true) => ({ id, current, attention, disposition, sourceKind: 'aside_static', providerId: 'aside.diff-facts', severity: { normalized: 'unknown', original: null }, validation: 'not_required' });

test('file header and cited reasons stay visible before findings load', async () => {
  const opened = await render({ threads: sessionView().threads });
  assert.match(text(), /Unresolved GitHub thread\s*— thread by bob on line 2/);
  assert.match(text(), /finding fnd-measured · provider aside.diff-facts · rule auth_path/);
  assert.match(text(), /Loading findings/);
  await click('Review suggested file src/auth.rs');
  assert.deepEqual(opened, [['f-auth']]);
});

test('only current active attention entries display in server order and deep-link', async () => {
  const queue = queueView();
  const first = queue.queue.items[0];
  const extras = Array.from({ length: 3 }, (_, i) => ({ ...findings[0], finding: { ...findings[0].finding, id: `extra-${i}`, title: `Extra ${i}` } }));
  first.findings = [entry('fnd-hint'), entry('fnd-measured'), ...extras.map((f) => entry(f.finding.id)), entry('fnd-external', 'dismissed_local'), entry('missing'), entry('historical', 'untouched', 'high', false)];
  const opened = await render({ queue, findings: [...findings, ...extras] });
  assert.match(text(), /Consider checking token reuse/);
  assert.match(text(), /L2 — Consider checking token reuse/);
  assert.match(text(), /L2 — Changes 2 lines in an auth path/);
  assert.match(text(), /High attention from a deterministic rule/);
  assert.match(text(), /\+2 more/);
  assert.doesNotMatch(text(), /CodeRabbit said tokens may leak/);
  assert.doesNotMatch(text(), /Extra 1/);
  await click('Review finding Consider checking token reuse in src/auth.rs');
  assert.deepEqual(opened, [['f-auth', 'fnd-hint']]);
});

test('empty and loading messages do not claim complete coverage prematurely', async () => {
  await render({ findings: [], session: sessionView({ overall: 'incomplete' }) });
  assert.match(text(), /not an all-clear/);
});
