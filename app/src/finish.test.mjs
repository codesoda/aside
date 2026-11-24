import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';
import { createRequire } from 'node:module';
import { installDom } from '../test/dom.mjs';
installDom();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const require = createRequire(import.meta.url);
const React = require('react');
const { createRoot } = require('react-dom/client');
const { act } = React;
const { createServer } = await import('vite');
const vite = await createServer({ server: { middlewareMode: true, hmr: false }, appType: 'custom' });
const { default: FinishReview } = await vite.ssrLoadModule('/src/workspace/FinishReview.tsx');
let root, view, calls, routes;
const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
const plan = (extra = {}) => ({ planId: 'plan-1', headSha: view.pr.headSha, baseSha: view.pr.baseSha, verdict: view.review.verdict, summary: view.review.summary, selectionRevision: view.review.selectionRevision, inline: [{ commentId: 'c1', threadId: 't1', authorKind: 'human', path: 'a.ts', side: 'RIGHT', line: 4, startLine: null, body: 'Exact body' }, { commentId: 'cross', threadId: 't2', authorKind: 'human', path: 'other.ts', side: 'LEFT', line: 8, startLine: 6, body: 'Cross-file body' }], replies: [{ commentId: 'reply', threadId: 't1', authorKind: 'human', path: 'a.ts', side: 'RIGHT', line: 4, parentCommentId: 'c1', body: 'Exact reply' }], omitted: [{ commentId: 'local', kind: 'reply', selected: false, code: 'not_selected', reason: 'Kept local by choice' }], ...extra });
const button = (text) => [...document.querySelectorAll('button')].find((node) => node.textContent.trim() === text);
const requests = (path) => calls.filter((call) => call.path === path);
const publish = '/api/v1/review/publish';
const previewPath = `${publish}/preview`;
const finishPath = '/api/v1/review/finish';
async function settle() { await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); }); }
async function click(text) { const node = button(text); assert.ok(node, text); await act(async () => node.click()); await settle(); }
async function change(node, value) { assert.ok(node); await act(async () => { const prototype = node.tagName === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype : window.HTMLSelectElement.prototype; Object.getOwnPropertyDescriptor(prototype, 'value').set.call(node, value); node.dispatchEvent(new window.Event(node.tagName === 'TEXTAREA' ? 'input' : 'change', { bubbles: true })); }); }
async function update(updated) { view = updated; await act(async () => root.render(React.createElement(FinishReview, { session: view, onSession: update }))); }
async function render() { await update(view); await settle(); }
const outcome = (status, extra = {}) => ({ attemptId: 'a1', status, githubReviewId: null, publishedCommentIds: [], remainingCommentIds: ['c1'], omittedCommentIds: ['local'], error: null, ...extra });
beforeEach(() => {
  view = { demo: false, pr: { headSha: 'abc', baseSha: 'def' }, review: { verdict: null, summary: '', selectionRevision: 0, unresolvedPublication: null } };
  calls = []; routes = {};
  globalThis.fetch = async (input, init = {}) => { const path = String(input); const body = init.body && JSON.parse(init.body); calls.push({ path, method: init.method ?? 'GET', body }); return (routes[path] ?? (() => json({ error: 'not_found', message: path }, 404)))(body); };
  routes['/api/v1/session'] = () => json(view);
  routes[`${publish}/status`] = () => json({ active: view.review.unresolvedPublication ? { attemptId: view.review.unresolvedPublication.attemptId, affectedDraftIds: ['c1', 'cross'] } : null });
  routes[finishPath] = ({ verdict, summary }) => { view = { ...view, review: { ...view.review, verdict, summary } }; return json({ review: view.review, sequence: 1 }); };
  routes[previewPath] = () => json({ plan: plan(), blockers: [], sequence: 2 });
  document.body.innerHTML = ''; root = createRoot(document.body);
});
afterEach(async () => { await act(async () => root.unmount()); });
test.after(async () => vite.close());

test('persisted values initialize the form; local edits survive incoming session changes', async () => {
  view.review.verdict = 'APPROVE'; view.review.summary = 'Persisted summary';
  await render();
  assert.equal(document.querySelector('select').value, 'APPROVE');
  assert.equal(document.querySelector('textarea').value, 'Persisted summary');
  await change(document.querySelector('textarea'), 'Unsaved summary');
  await change(document.querySelector('select'), 'REQUEST_CHANGES');
  await update({ ...view, review: { ...view.review, summary: 'Other tab', verdict: 'COMMENT', selectionRevision: 1 } });
  assert.equal(document.querySelector('textarea').value, 'Unsaved summary');
  assert.equal(document.querySelector('select').value, 'REQUEST_CHANGES');
  await click('Finish and preview');
  assert.deepEqual(requests(finishPath)[0].body, { verdict: 'REQUEST_CHANGES', summary: 'Unsaved summary' });
});

test('exact cross-file preview and selected reply/omission are visible; trimmed payload is identical through confirmation', async () => {
  routes[publish] = () => json({ outcome: outcome('succeeded', { publishedCommentIds: ['c1', 'cross', 'reply'], remainingCommentIds: [] }) });
  await render(); await change(document.querySelector('textarea'), '  Review text  ');
  assert.equal(requests(publish).length, 0);
  await click('Finish and preview');
  assert.deepEqual(requests(finishPath)[0].body, { verdict: 'COMMENT', summary: 'Review text' });
  assert.deepEqual(requests(previewPath)[0].body, { verdict: 'COMMENT', body: 'Review text' });
  const text = document.querySelector('[aria-label="Publication preview"]').textContent;
  for (const piece of ['a.ts', 'RIGHT', 'Exact body', 'other.ts', 'LEFT', 'lines 6–8', 'Cross-file body', 'Exact reply', 'local', 'not_selected', 'Kept local by choice']) assert.ok(text.includes(piece), piece);
  assert.equal(requests(publish).length, 0);
  await click('Publish to GitHub');
  assert.deepEqual(requests(publish)[0].body, { planId: 'plan-1', verdict: 'COMMENT', body: 'Review text', confirm: true });
  assert.equal(requests(publish).length, 1);
});

test('blockers and demo disable confirmation without publication', async () => {
  routes[previewPath] = () => json({ plan: plan(), blockers: [{ code: 'empty_review', message: 'Add a summary' }] });
  await render(); await click('Finish and preview');
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.match(document.body.textContent, /empty_review: Add a summary/);
  await update({ ...view, demo: true });
  assert.equal(button('Finish and preview').disabled, true);
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.equal(requests(publish).length, 0);
});

test('editing summary and verdict or changing persisted fingerprint stales exact preview but keeps local form', async () => {
  await render(); await click('Finish and preview');
  await change(document.querySelector('textarea'), 'Local draft');
  assert.equal(button('Publish to GitHub').disabled, true);
  await click('Finish and preview');
  assert.equal(requests(finishPath).at(-1).body.summary, 'Local draft');
  assert.equal(button('Publish to GitHub').disabled, false);
  await change(document.querySelector('select'), 'APPROVE');
  assert.equal(button('Publish to GitHub').disabled, true);
  await click('Finish and preview');
  await update({ ...view, review: { ...view.review, verdict: 'COMMENT', summary: 'Other tab summary', selectionRevision: 3 } });
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.equal(document.querySelector('textarea').value, 'Local draft');
  assert.equal(document.querySelector('select').value, 'APPROVE');
  assert.equal(requests(publish).length, 0);
});

test('plan conflict refuses retry until a fresh exact preview', async () => {
  routes[publish] = () => json({ error: 'plan_changed', message: 'Changed' }, 409);
  await render(); await click('Finish and preview'); await click('Publish to GitHub');
  assert.match(document.body.textContent, /Make a new preview before publishing/);
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.equal(requests(publish).length, 1);
  routes[publish] = () => json({ outcome: outcome('partial', { publishedCommentIds: ['c1'], remainingCommentIds: ['cross'], error: 'unknown' }) });
  await click('Finish and preview'); await click('Publish to GitHub');
  assert.match(document.body.textContent, /Remaining unsent: cross/);
  assert.match(document.body.textContent, /will not retry/);
  assert.equal(button('Finish and preview').disabled, true);
  assert.equal(requests(publish).length, 2);
});

test('partial response locks even when status has no active attempt', async () => {
  routes[publish] = () => json({ outcome: outcome('partial', { error: 'unknown' }) });
  await render(); await click('Finish and preview'); await click('Publish to GitHub');
  assert.equal(button('Finish and preview').disabled, true);
  assert.match(document.body.textContent, /No authoritative active attempt ID/);
  assert.equal(requests(publish).length, 1);
});

test('rejected response preserves editable local summary', async () => {
  routes[publish] = () => json({ outcome: outcome('rejected', { error: 'stale plan' }) });
  await render(); await change(document.querySelector('textarea'), 'Keep this'); await click('Finish and preview'); await click('Publish to GitHub');
  assert.match(document.body.textContent, /stale plan/);
  assert.equal(document.querySelector('textarea').value, 'Keep this');
  assert.equal(document.querySelector('textarea').disabled, false);
});

test('verified reconciliation sends active attempt and refreshes authority before unlock', async () => {
  view.review.unresolvedPublication = { attemptId: 'a1', status: 'partial' };
  routes[`${publish}/reconcile`] = (body) => { view = { ...view, review: { ...view.review, unresolvedPublication: null } }; return json({ sequence: 3 }); };
  await render();
  assert.equal(button('Finish and preview').disabled, true);
  assert.equal(button('Confirm checked outcome').disabled, true);
  assert.match(document.body.textContent, /Affected draft IDs: c1, cross/);
  await change(document.querySelector('[aria-label="Verified publication outcome"]'), 'not_posted');
  await click('Confirm checked outcome');
  assert.deepEqual(requests(`${publish}/reconcile`)[0].body, { attemptId: 'a1', outcome: 'not_posted', confirm: true });
  assert.ok(requests(`${publish}/status`).length >= 2);
  assert.equal(button('Finish and preview').disabled, false);
  assert.equal(requests(publish).length, 0);
});

test('new active attempt clears a previously verified choice', async () => {
  view.review.unresolvedPublication = { attemptId: 'a1', status: 'partial' };
  await render();
  await change(document.querySelector('[aria-label="Verified publication outcome"]'), 'posted');
  assert.equal(button('Confirm checked outcome').disabled, false);
  await update({ ...view, review: { ...view.review, unresolvedPublication: { attemptId: 'a2', status: 'partial' } } }); await settle();
  assert.match(document.body.textContent, /Attempt: a2/);
  assert.equal(document.querySelector('[aria-label="Verified publication outcome"]').value, '');
  assert.equal(button('Confirm checked outcome').disabled, true);
  assert.equal(requests(`${publish}/reconcile`).length, 0);
});

test('failed reconciliation stays locked and retains verified attempt', async () => {
  view.review.unresolvedPublication = { attemptId: 'a1', status: 'partial' };
  routes[`${publish}/reconcile`] = () => json({ error: 'conflict', message: 'Already posted' }, 409);
  await render(); await change(document.querySelector('[aria-label="Verified publication outcome"]'), 'posted'); await click('Confirm checked outcome');
  assert.match(document.body.textContent, /Already posted/);
  assert.equal(button('Finish and preview').disabled, true);
  assert.equal(requests(publish).length, 0);
});

test('transport uncertainty without authoritative claim cannot be retried or reconciled', async () => {
  routes[publish] = () => { throw new Error('connection lost'); };
  await render(); await change(document.querySelector('textarea'), 'Keep this'); await click('Finish and preview'); await click('Publish to GitHub');
  assert.equal(button('Finish and preview').disabled, true);
  assert.match(document.body.textContent, /No authoritative active attempt ID/);
  assert.equal(document.querySelector('textarea').value, 'Keep this');
  assert.equal(button('Confirm checked outcome'), undefined);
  assert.equal(requests(publish).length, 1);
});

test('status lookup failure fails closed until a successful explicit check', async () => {
  routes[`${publish}/status`] = () => json({ error: 'unavailable', message: 'unavailable' }, 503);
  await render();
  assert.equal(button('Finish and preview').disabled, true);
  assert.match(document.body.textContent, /Could not check publication status/);
  routes[`${publish}/status`] = () => json({ active: null });
  await click('Check publication status');
  assert.equal(button('Finish and preview').disabled, false);
});

test('preview failure preserves unsaved text without publishing', async () => {
  routes[previewPath] = () => json({ error: 'unavailable', message: 'Preview unavailable' }, 503);
  await render(); await change(document.querySelector('textarea'), 'Keep me'); await click('Finish and preview');
  assert.match(document.body.textContent, /Preview unavailable/);
  assert.equal(document.querySelector('textarea').value, 'Keep me');
  assert.equal(button('Publish to GitHub'), undefined);
  assert.equal(requests(publish).length, 0);
});

test('uncertain transport with active claim permits only explicit reconciliation', async () => {
  routes[publish] = () => { view = { ...view, review: { ...view.review, unresolvedPublication: { attemptId: 'a9', status: 'partial' } } }; throw new Error('connection lost'); };
  await render(); await click('Finish and preview'); await click('Publish to GitHub');
  assert.equal(button('Finish and preview').disabled, true);
  assert.match(document.body.textContent, /Attempt: a9/);
  assert.equal(button('Confirm checked outcome').disabled, true);
  assert.equal(requests(publish).length, 1);
});

test('pending publish does not send twice', async () => {
  let resolve;
  routes[publish] = () => new Promise((done) => { resolve = done; });
  await render(); await click('Finish and preview');
  await act(async () => { button('Publish to GitHub').click(); });
  assert.equal(requests(publish).length, 1);
  assert.equal(button('Publish to GitHub').disabled, true);
  await act(async () => resolve(json({ outcome: outcome('succeeded') })));
  await settle();
  assert.equal(requests(publish).length, 1);
});
