import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
installDom();
const { mount, unmount, flushSync } = await import('svelte');
const { proxy } = await import('svelte/internal/client');
const { default: ConversationPanel } = await import('./workspace/ConversationPanel.svelte');
let app;
const originalFetch = globalThis.fetch;
afterEach(() => { if (app) unmount(app); app = null; document.body.innerHTML = ''; globalThis.fetch = originalFetch; });
function deferredPromise() { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
function thread(id, reviewComment = null) { return { id, source: 'local', subject: 'file', line: null, side: 'RIGHT', anchorState: 'current', comments: [], reviewComment, agent: null, resolved: false }; }
function setup() {
  const drafts = proxy({ A: { reply: 'A reply', outgoing: 'A outgoing', editing: true }, B: { reply: 'B unsent', outgoing: 'B outgoing', editing: true } });
  const props = proxy({ thread: thread('A', { id: 'saved-A', body: 'A saved' }), drafts, onRefresh: async () => {}, onError: () => {}, onClose: () => {} });
  const calls = [], errors = [];
  props.onError = (message) => errors.push(message);
  globalThis.fetch = (url, init) => { const pending = deferredPromise(); calls.push({ url, init, pending }); return pending.promise; };
  app = mount(ConversationPanel, { target: document.body, props }); flushSync();
  return { drafts, props, calls, errors, switchB() { props.thread = thread('B', { id: 'saved-B', body: 'B saved' }); flushSync(); } };
}
function click(label) { const button = [...document.querySelectorAll('button')].find((b) => b.textContent.includes(label)); assert.ok(button, label); button.click(); flushSync(); }
function input(label, value) { const node = document.querySelector(`[aria-label="${label}"]`); node.value = value; node.dispatchEvent(new window.Event('input', { bubbles: true })); flushSync(); }
async function settle() { await new Promise((resolve) => setTimeout(resolve, 0)); flushSync(); }

for (const operation of ['reply', 'save', 'clear']) {
  test(`pending A ${operation} completion does not mutate B draft`, async () => {
    const { drafts, calls, switchB } = setup();
    if (operation === 'reply') click('Send private reply');
    if (operation === 'save') click('Save review comment');
    if (operation === 'clear') click('Clear');
    assert.equal(calls.length, 1);
    assert.match(calls[0].url, /\/A\//);
    switchB();
    calls[0].pending.resolve({ ok: true }); await settle();
    assert.deepEqual(drafts.B, { reply: 'B unsent', outgoing: 'B outgoing', editing: true });
    if (operation === 'reply') assert.equal(drafts.A.reply, '');
    else assert.equal(drafts.A.outgoing, operation === 'clear' ? '' : 'A outgoing');
  });
}

test('native thread offers an outgoing reply without local resolution', () => {
  const { props } = setup();
  props.thread = { ...thread('A'), source: 'github', githubThreadId: 'GH_1' }; flushSync();
  assert.ok(document.querySelector('[aria-label="Review comment draft"]'));
  assert.equal([...document.querySelectorAll('button')].some((b) => b.textContent.includes('Mark handled locally')), false);
  assert.match(document.body.textContent, /reply/i);
});

test('clearing saved text deletes the draft once and never publishes', async () => {
  const { drafts, calls } = setup();
  input('Review comment draft', '');
  assert.equal(calls.length, 1);
  assert.equal(calls[0].init.method, 'DELETE');
  input('Review comment draft', '');
  assert.equal(calls.length, 1);
  calls[0].pending.resolve({ ok: true }); await settle();
  assert.equal(drafts.A.outgoing, '');
});

test('failed clear keeps blank input and reports the error', async () => {
  const { drafts, calls, errors } = setup();
  input('Review comment draft', '');
  calls[0].pending.reject(new Error('delete failed')); await settle();
  assert.equal(drafts.A.outgoing, '');
  assert.equal(document.querySelector('[aria-label="Review comment draft"]').value, '');
  assert.deepEqual(errors, ['delete failed']);
});

test('canceling agent copy preserves unsaved outgoing text', () => {
  const { props, drafts, calls } = setup();
  props.thread = { ...thread('A'), comments: [{ id: 'agent-1', author: 'Agent', authorKind: 'agent', publishState: 'private', body: 'Suggested wording' }] }; flushSync();
  const previous = globalThis.confirm;
  globalThis.confirm = () => false;
  try { click('Add to review'); } finally { globalThis.confirm = previous; }
  assert.equal(drafts.A.outgoing, 'A outgoing');
  assert.equal(calls.length, 0);
});

test('editing A while save is pending retains newer text', async () => {
  const { drafts, calls } = setup();
  click('Save review comment');
  input('Review comment draft', 'New A text');
  calls[0].pending.resolve({ ok: true }); await settle();
  assert.equal(drafts.A.outgoing, 'New A text');
  assert.equal(drafts.A.editing, true);
});

test('failed A request does not alter B draft and releases pending state', async () => {
  const { drafts, calls, errors, switchB } = setup();
  click('Send private reply'); switchB();
  assert.equal(document.querySelector('button[type="submit"]').disabled, false);
  calls[0].pending.reject(new Error('network failure')); await settle();
  assert.deepEqual(drafts.B, { reply: 'B unsent', outgoing: 'B outgoing', editing: true });
  assert.deepEqual(errors, []);
  assert.equal(document.querySelector('button[type="submit"]').disabled, false);
});
