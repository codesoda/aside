import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
installDom();
const { mount, unmount, flushSync } = await import('svelte');
const { proxy } = await import('svelte/internal/client');
const { default: SubmitReview } = await import('./workspace/SubmitReview.svelte');
let app;
afterEach(() => { if (app) unmount(app); app = null; document.body.innerHTML = ''; });
const threads = [{ id: 't1', path: 'a.ts', resolved: true, reviewComment: { id: 'c1', body: 'Fix this', blocker: null, locked: false } }, { id: 't2', resolved: false, reviewComment: null }];
const plan = (verdict, body, extra = {}) => ({ planId: 'plan-1', verdict, summary: body, inline: [{ commentId: 'c1', threadId: 't1', path: 'a.ts', side: 'RIGHT', line: 4, startLine: null, body: 'Fix this' }], replies: [], omitted: [], preparedCount: 1, openCount: 1, resolvedCount: 1, headSha: 'h', baseSha: 'b', attemptOrdinal: 1, ...extra });
function render(extra = {}) {
  const previews = [], submissions = [];
  app = mount(SubmitReview, { target: document.body, props: { threads, onPreview: async (arg) => { previews.push(arg); return { plan: plan(arg.verdict, arg.body), sequence: 1 }; }, onSubmit: async (arg) => { submissions.push(arg); return { outcome: { attemptId: 'attempt-1', status: 'succeeded', githubReviewId: 12, publishedCommentIds: ['c1'], remainingCommentIds: [], error: null }, sequence: 2 }; }, onLookup: async () => ({ active: null }), ...extra } });
  flushSync(); return { previews, submissions };
}
function button(text) { const node = [...document.querySelectorAll('button')].find((item) => item.textContent.includes(text)); assert.ok(node, text); return node; }
function click(text) { button(text).click(); flushSync(); }
function change(selector, value, event = 'input') { const node = document.querySelector(selector); node.value = value; node.dispatchEvent(new window.Event(event, { bubbles: true })); flushSync(); }
async function settle() { await new Promise((resolve) => setTimeout(resolve, 0)); flushSync(); }

test('opening and editing do not submit; exact preview and explicit confirmation submit once', async () => {
  const { previews, submissions } = render();
  await settle(); click('Submit review');
  assert.match(document.body.textContent, /1 prepared review comments · 1 on resolved threads/);
  assert.match(document.body.textContent, /Prepared comments on resolved threads remain/);
  change('textarea', 'Overall');
  assert.equal(previews.length, 0); assert.equal(submissions.length, 0);
  click('Preview exact submission'); await settle();
  assert.deepEqual(previews, [{ verdict: 'COMMENT', body: 'Overall' }]);
  assert.match(document.querySelector('[aria-label="Exact outgoing preview"]').textContent, /a.ts:4.*Fix this/);
  change('textarea', 'Updated');
  assert.equal(document.querySelector('[aria-label="Exact outgoing preview"]'), null);
  click('Preview exact submission'); await settle();
  click('Confirm and submit review'); await settle();
  assert.deepEqual(submissions, [{ planId: 'plan-1', verdict: 'COMMENT', body: 'Updated', confirm: true }]);
  assert.match(document.querySelector('[role="status"]').textContent, /submitted successfully/);
  click('Submit review');
  assert.equal(document.querySelector('textarea').value, '');
  assert.equal(document.querySelector('select').value, 'COMMENT');
});

test('missing body, known self approval and blocked statuses are visible', async () => {
  const { previews } = render({ selfReview: true, threads: [{ ...threads[0], reviewComment: { ...threads[0].reviewComment, blocker: 'stale anchor' } }] });
  click('Submit review');
  assert.match(document.body.textContent, /stale anchor/);
  assert.equal(button('Preview exact submission').disabled, true);
  change('textarea', 'A comment');
  change('select', 'APPROVE', 'change');
  assert.equal(button('Preview exact submission').disabled, true);
  assert.equal(previews.length, 0);
});

test('mismatched preview prevents confirmation; preview errors preserve draft', async () => {
  let calls = 0;
  render({ onPreview: async ({ verdict, body }) => { calls++; if (calls === 1) return { plan: plan(verdict, body, { preparedCount: 2 }), sequence: 1 }; throw new Error('Preview unavailable'); } });
  await settle(); click('Submit review'); change('textarea', 'Keep me'); click('Preview exact submission'); await settle();
  assert.match(document.querySelector('[role="alert"]').textContent, /does not match/);
  assert.equal([...document.querySelectorAll('button')].some((item) => item.textContent.includes('Confirm and submit review')), false);
  click('Preview exact submission'); await settle();
  assert.match(document.querySelector('[role="alert"]').textContent, /Preview unavailable/);
  assert.equal(document.querySelector('textarea').value, 'Keep me');
});

test('pending calls reject double submit; partial outcome locks further action', async () => {
  let resolve;
  const submissions = [];
  render({ onSubmit: (arg) => { submissions.push(arg); return new Promise((done) => { resolve = done; }); } });
  await settle(); click('Submit review'); change('textarea', 'Body'); click('Preview exact submission'); await settle();
  const confirm = button('Confirm and submit review');
  confirm.click(); flushSync(); confirm.click(); flushSync();
  assert.equal(submissions.length, 1);
  resolve({ outcome: { attemptId: 'attempt-2', status: 'partial', githubReviewId: 42, publishedCommentIds: ['c1'], remainingCommentIds: [], error: 'uncertain' }, sequence: 2 }); await settle();
  assert.match(document.querySelector('[role="status"]').textContent, /GitHub review: 42/);
  assert.equal(button('Preview exact submission').disabled, true);
  click('Close');
  assert.equal(document.querySelector('.popover'), null);
  assert.equal(button('Submit review').disabled, false);
  click('Submit review');
  assert.match(document.querySelector('[role="status"]').textContent, /attempt-2/);
  assert.equal(button('Preview exact submission').disabled, true);
  document.querySelector('.popover').dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); flushSync();
  assert.equal(document.querySelector('.popover'), null);
  assert.equal(submissions.length, 1);
});

test('updated prepared draft invalidates exact preview without submitting', async () => {
  const reactiveThreads = proxy(structuredClone(threads));
  const { submissions } = render({ threads: reactiveThreads });
  await settle(); click('Submit review'); change('textarea', 'Overall'); click('Preview exact submission'); await settle();
  assert.ok(document.querySelector('[aria-label="Exact outgoing preview"]'));
  reactiveThreads[0].reviewComment.body = 'Changed after preview'; flushSync();
  assert.equal(document.querySelector('[aria-label="Exact outgoing preview"]'), null);
  assert.equal(submissions.length, 0);
});

test('missing reviewComment does not count as prepared', () => {
  render({ threads: [{ ...threads[0], reviewComment: undefined }, threads[1]] });
  assert.match(button('Submit review').textContent, /Draft \(0\)/);
});

test('rejected response preserves editable draft', async () => {
  render({ onSubmit: async () => ({ outcome: { attemptId: 'r1', status: 'rejected', githubReviewId: null, publishedCommentIds: [], remainingCommentIds: ['c1'], error: 'stale plan' }, sequence: 2 }) });
  await settle(); click('Submit review'); change('textarea', 'Still here'); click('Preview exact submission'); await settle();
  click('Confirm and submit review'); await settle();
  assert.match(document.querySelector('[role="alert"]').textContent, /stale plan/);
  assert.equal(document.querySelector('textarea').value, 'Still here');
  assert.equal(document.querySelector('textarea').disabled, false);
});

test('reconciliation requires explicit selection and refreshes authority', async () => {
  let active = { attemptId: 'attempt-2', affectedDraftIds: ['c1'] };
  const calls = []; let refreshes = 0;
  render({ onLookup: async () => ({ active }), onReconcile: async (input) => { calls.push(input); active = null; return { sequence: 3 }; }, onReconciled: async () => { refreshes++; } });
  await settle(); click('Submit review'); await settle();
  assert.equal(button('Confirm checked outcome').disabled, true);
  assert.match(document.body.textContent, /Affected draft IDs: c1/);
  change('select[aria-label="Verified publication outcome"]', 'posted', 'change');
  click('Confirm checked outcome'); await settle();
  assert.deepEqual(calls, [{ attemptId: 'attempt-2', outcome: 'posted', confirm: true }]);
  assert.equal(refreshes, 1);
  assert.equal(button('Preview exact submission').disabled, true);
});

test('verified not-posted reconciliation requires consent and retains the body', async () => {
  let active = null;
  const calls = [];
  render({ onSubmit: async () => ({ outcome: { attemptId: 'attempt-4', status: 'partial', githubReviewId: null, publishedCommentIds: [], remainingCommentIds: ['c1'], error: 'unknown' }, sequence: 2 }), onLookup: async () => ({ active }), onReconcile: async (input) => { calls.push(input); active = null; return { sequence: 3 }; } });
  await settle(); click('Submit review'); change('textarea', 'Do not lose this'); click('Preview exact submission'); await settle();
  click('Confirm and submit review'); await settle();
  assert.equal(button('Confirm checked outcome').disabled, true);
  change('select[aria-label="Verified publication outcome"]', 'not_posted', 'change');
  click('Confirm checked outcome'); await settle();
  assert.deepEqual(calls, [{ attemptId: 'attempt-4', outcome: 'not_posted', confirm: true }]);
  assert.equal(document.querySelector('textarea').value, 'Do not lose this');
  assert.equal(button('Preview exact submission').disabled, false);
});

test('a selected outcome cannot carry over to a different active attempt', async () => {
  let active = { attemptId: 'attempt-A', affectedDraftIds: ['c1'] };
  const calls = [];
  render({ onLookup: async () => ({ active }), onReconcile: async (input) => { calls.push(input); return { sequence: 3 }; } });
  await settle(); click('Submit review'); await settle();
  change('select[aria-label="Verified publication outcome"]', 'not_posted', 'change');
  assert.equal(button('Confirm checked outcome').disabled, false);
  click('Close');
  active = { attemptId: 'attempt-B', affectedDraftIds: ['c1'] };
  click('Submit review'); await settle();
  assert.match(document.body.textContent, /Attempt: attempt-B/);
  assert.equal(document.querySelector('select[aria-label="Verified publication outcome"]').value, '');
  assert.equal(button('Confirm checked outcome').disabled, true);
  assert.deepEqual(calls, []);
});

test('failed not-posted reconciliation stays locked', async () => {
  render({ onLookup: async () => ({ active: { attemptId: 'attempt-3', affectedDraftIds: ['c1'] } }), onReconcile: async () => { throw new Error('Conflict'); } });
  await settle(); click('Submit review'); await settle();
  change('select[aria-label="Verified publication outcome"]', 'not_posted', 'change');
  click('Confirm checked outcome'); await settle();
  assert.match(document.body.textContent, /Conflict/);
  assert.equal(button('Preview exact submission').disabled, true);
  click('Close'); click('Submit review'); await settle();
  assert.equal(button('Preview exact submission').disabled, true);
});

test('transport uncertainty without active claim cannot be reconciled or retried', async () => {
  render({ onSubmit: async () => { throw new Error('Connection lost'); } });
  await settle(); click('Submit review'); change('textarea', 'Keep this'); click('Preview exact submission'); await settle();
  click('Confirm and submit review'); await settle();
  assert.match(document.body.textContent, /No active attempt could be identified/);
  assert.equal(document.querySelector('textarea').value, 'Keep this');
  assert.equal(button('Preview exact submission').disabled, true);
  assert.equal([...document.querySelectorAll('button')].some((item) => item.textContent.includes('Confirm checked outcome')), false);
});

test('Escape closes without submitting', () => {
  const { submissions } = render(); click('Submit review');
  document.querySelector('.popover').dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); flushSync();
  assert.equal(document.querySelector('.popover'), null); assert.equal(submissions.length, 0);
});
