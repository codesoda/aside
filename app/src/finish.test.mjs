import assert from 'node:assert/strict';
import { beforeEach, afterEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
import { sessionView } from '../test/fixtures.mjs';
installDom();
const { mount, unmount, tick, flushSync } = await import('svelte');
const { default: App } = await import('./App.svelte');
const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
let app, view, calls, routes, events;
class Events {
  constructor() { events = this; }
  listeners = {};
  addEventListener(name, fn) { (this.listeners[name] ??= []).push(fn); }
  close() {}
}
async function settle() { for (let i = 0; i < 8; i++) { await new Promise((r) => setTimeout(r, 0)); await tick(); } flushSync(); }
const button = (label) => [...document.querySelectorAll('button')].find((item) => item.textContent.trim() === label);
const plan = () => ({ planId: 'plan-1', headSha: 'abc', baseSha: 'def', verdict: view.review.verdict, summary: view.review.summary, selectionRevision: view.review.selectionRevision,
  inline: [{ commentId: 'local', threadId: 't-local', authorKind: 'human', path: 'src/auth.rs', side: 'RIGHT', line: 2, startLine: null, body: 'Exact root body' }],
  replies: [{ commentId: 'reply', threadId: 't-local', authorKind: 'human', path: 'src/auth.rs', side: 'RIGHT', line: 2, parentCommentId: 'local', githubParentId: null, body: 'Exact reply body' }],
  omitted: [{ commentId: 'other', threadId: 't-local', kind: 'reply', selected: false, code: 'not_selected', reason: 'Kept local by choice' }] });
beforeEach(() => {
  view = sessionView(); view.demo = false; view.review = { lifecycle: 'in-progress', verdict: null, summary: '', finishedAt: null, publishedAt: null, selectionRevision: 0, unresolvedPublication: null };
  view.threads.push({ ...view.threads[0], id: 't-local', source: 'local', comments: [
    { ...view.threads[0].comments[0], id: 'local', threadId: 't-local', body: 'Exact root body', publishState: 'draft', publishSelected: true },
    { ...view.threads[0].comments[0], id: 'reply', threadId: 't-local', body: 'Exact reply body', publishState: 'draft', publishSelected: true },
  ] });
  calls = []; routes = {}; events = null;
  globalThis.EventSource = Events;
  globalThis.fetch = async (input, init = {}) => { const path = new URL(String(input), 'http://localhost').pathname; const method = init.method ?? 'GET'; const body = init.body && JSON.parse(init.body); calls.push({ path, method, body }); return (routes[`${method} ${path}`] ?? (() => json({ error: 'not_found', message: path }, 404)))(body); };
  routes['GET /api/v1/session'] = () => json(view);
  routes['GET /api/v1/analysis/queue'] = () => json({ queue: { items: [], coverage: 'pending', verdict: { kind: 'remaining', count: 0 } }, counts: {} });
  routes['GET /api/v1/analysis/findings'] = () => json({ items: [], hasMore: false, nextCursor: null });
  routes['POST /api/v1/review/publish/selection'] = () => { view.review.selectionRevision++; return json({ selected: false, changed: ['local'], selectionRevision: view.review.selectionRevision, sequence: 2 }); };
  routes['POST /api/v1/review/finish'] = ({ verdict, summary }) => { view.review.verdict = verdict; view.review.summary = summary; view.review.lifecycle = 'finished'; return json({ review: view.review, sequence: 3 }); };
  routes['POST /api/v1/review/publish/preview'] = () => json({ plan: plan(), blockers: [], sequence: 4 });
  document.body.innerHTML = ''; app = null;
});
afterEach(() => { if (app) unmount(app); });
async function start() { app = mount(App, { target: document.body }); await settle(); }
async function preview() { button('Finish review').click(); await settle(); button('Finish and preview').click(); await settle(); }
test('draft and thread controls make one batch each, and preview shows exact destinations, bodies and omissions', async () => {
  await start(); document.querySelector('[aria-controls="workspace-files"]').click(); await settle();
  const boxes = [...document.querySelectorAll('input[type=checkbox]')].filter((input) => input.parentElement.textContent.includes('publishing'));
  assert.equal(boxes.length, 3);
  boxes[1].click(); await settle();
  assert.deepEqual(calls.find((call) => call.path.endsWith('/selection')).body, { commentIds: ['local'], selected: false });
  boxes[0].click(); await settle();
  assert.deepEqual(calls.filter((call) => call.path.endsWith('/selection'))[1].body, { commentIds: ['local', 'reply'], selected: false });
  await preview();
  const text = document.body.textContent;
  for (const piece of ['src/auth.rs', 'RIGHT', 'Exact root body', 'Exact reply body', 'other', 'not_selected', 'Kept local by choice']) assert.ok(text.includes(piece), piece);
  assert.equal(calls.filter((call) => call.path === '/api/v1/review/publish').length, 0);
});
test('blockers and demo disable confirmation; revision change stales preview', async () => {
  routes['POST /api/v1/review/publish/preview'] = () => json({ plan: plan(), blockers: [{ code: 'empty_review', message: 'Add a summary' }], sequence: 4 });
  await start(); await preview();
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.match(document.body.textContent, /empty_review: Add a summary/);
  view.review.selectionRevision++; events.listeners['review.selection.changed'][0](); await settle();
  assert.match(document.body.textContent, /preview is stale/);
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.equal(calls.filter((call) => call.path === '/api/v1/review/publish').length, 0);
});
test('editing verdict or summary invalidates the exact preview until a new one is made', async () => {
  await start(); await preview();
  assert.equal(button('Publish to GitHub').disabled, false);
  const summary = document.querySelector('#workspace-finish textarea');
  summary.value = 'Changed after preview'; summary.dispatchEvent(new window.Event('input', { bubbles: true })); await settle();
  assert.match(document.body.textContent, /preview is stale/);
  assert.equal(button('Publish to GitHub').disabled, true);
  button('Finish and preview').click(); await settle();
  assert.equal(calls.filter((call) => call.path === '/api/v1/review/finish').at(-1).body.summary, 'Changed after preview');
  assert.equal(button('Publish to GitHub').disabled, false);
  const verdict = document.querySelector('#workspace-finish select');
  verdict.value = 'APPROVE'; verdict.dispatchEvent(new window.Event('change', { bubbles: true })); await settle();
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.equal(calls.filter((call) => call.path === '/api/v1/review/publish').length, 0);
});

test('review.finished with changed persisted verdict and summary stales preview without overwriting local form', async () => {
  view.review.summary = 'Review summary';
  await start(); await preview();
  assert.equal(button('Publish to GitHub').disabled, false);
  view.review.verdict = 'APPROVE'; view.review.summary = 'Other tab summary';
  events.listeners['review.finished'][0](); await settle();
  assert.match(document.body.textContent, /preview is stale/);
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.equal(document.querySelector('#workspace-finish select').value, 'COMMENT');
  assert.equal(document.querySelector('#workspace-finish textarea').value, 'Review summary');
  const summary = document.querySelector('#workspace-finish textarea');
  summary.value = 'Unsaved local summary'; summary.dispatchEvent(new window.Event('input', { bubbles: true })); await settle();
  button('Review prep').click(); await settle(); button('Finish review').click(); await settle();
  assert.equal(document.querySelector('#workspace-finish textarea').value, 'Unsaved local summary');
  assert.equal(calls.filter((call) => call.path === '/api/v1/review/publish').length, 0);
});

test('finish form keeps unsaved edits across tabs and session refreshes', async () => {
  await start(); button('Finish review').click(); await settle();
  const summary = document.querySelector('#workspace-finish textarea');
  const verdict = document.querySelector('#workspace-finish select');
  summary.value = 'Unsaved summary'; summary.dispatchEvent(new window.Event('input', { bubbles: true }));
  verdict.value = 'REQUEST_CHANGES'; verdict.dispatchEvent(new window.Event('change', { bubbles: true })); await settle();
  button('Review prep').click(); await settle();
  events.listeners['review.selection.changed'][0](); await settle();
  button('Finish review').click(); await settle();
  assert.equal(document.querySelector('#workspace-finish textarea').value, 'Unsaved summary');
  assert.equal(document.querySelector('#workspace-finish select').value, 'REQUEST_CHANGES');
  button('Finish and preview').click(); await settle();
  assert.deepEqual(calls.find((call) => call.path === '/api/v1/review/finish').body, { verdict: 'REQUEST_CHANGES', summary: 'Unsaved summary' });
});

test('finish form starts with the persisted review values', async () => {
  view.review.verdict = 'APPROVE'; view.review.summary = 'Persisted summary';
  await start(); button('Finish review').click(); await settle();
  assert.equal(document.querySelector('#workspace-finish select').value, 'APPROVE');
  assert.equal(document.querySelector('#workspace-finish textarea').value, 'Persisted summary');
});

test('demo can preview locally but cannot confirm', async () => {
  view.demo = true;
  await start(); await preview();
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.match(document.body.textContent, /Demo is local only/);
  assert.equal(calls.filter((call) => call.path === '/api/v1/review/publish').length, 0);
});

test('stale confirmation refuses retry and partial results remain visible', async () => {
  routes['POST /api/v1/review/publish'] = () => json({ error: 'plan_changed', message: 'Changed' }, 409);
  await start(); await preview(); button('Publish to GitHub').click(); await settle();
  assert.match(document.body.textContent, /Make a new preview before publishing/);
  assert.equal(button('Publish to GitHub').disabled, true);
  assert.deepEqual(calls.find((call) => call.path === '/api/v1/review/publish').body, { planId: 'plan-1', confirm: true });
  routes['POST /api/v1/review/publish'] = () => json({ outcome: { status: 'partial', attemptId: 'a1', githubReviewId: null, publishedCommentIds: ['local'], remainingCommentIds: ['reply'], omittedCommentIds: ['other'], error: 'unknown' }, sequence: 5 });
  button('Finish and preview').click(); await settle(); button('Publish to GitHub').click(); await settle();
  assert.match(document.body.textContent, /Remaining unsent: reply/);
  assert.match(document.body.textContent, /will not retry/);
});
