// Promotion from the mounted review stays local and opens the existing draft editor.
import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
import { createRequire } from 'node:module';
import { findings, page, queueView, sessionView } from '../test/fixtures.mjs';

installDom();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const require = createRequire(import.meta.url);
const React = require('react');
const { act } = React;
const { createRoot } = require('react-dom/client');
const { createServer } = await import('vite');
const vite = await createServer({ server: { middlewareMode: true, hmr: false }, appType: 'custom' });
const { default: App } = await vite.ssrLoadModule('/src/App.tsx');
test.after(async () => vite.close());
const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
const path = `/api/v1/analysis/findings/${findings[0].finding.id}/promote`;
let app;
let calls;
let routes;
let view;
let current;

class FakeEventSource {
  static latest;
  listeners = new Map();
  constructor() { FakeEventSource.latest = this; }
  addEventListener(name, listener) { this.listeners.set(name, listener); }
  emit(name) { this.listeners.get(name)?.(); }
  close() {}
}

beforeEach(() => {
  document.body.innerHTML = '';
  app = createRoot(document.body);
  calls = [];
  view = sessionView();
  current = structuredClone(findings[0]);
  current.disposition = 'untouched';
  current.userState = { revision: 3, disposition: 'untouched', snoozedUntil: null, promotedDraftId: null };
  current.canPromote = true;
  routes = {
    'GET /api/v1/session': () => json(view),
    'GET /api/v1/analysis/queue': () => json(queueView()),
    'GET /api/v1/analysis/findings': () => json(page([current])),
  };
  globalThis.EventSource = FakeEventSource;
  globalThis.fetch = async (input, init = {}) => {
    const url = new URL(String(input), 'http://127.0.0.1');
    const method = init.method ?? 'GET';
    calls.push({ method, path: url.pathname, search: url.search, body: init.body });
    const handler = routes[`${method} ${url.pathname}`];
    return handler ? handler(url, init) : json({ error: 'not_found', message: `no route ${method} ${url.pathname}` }, 404);
  };
  document.title = 'Aside';
});

afterEach(async () => { if (app) await act(async () => app.unmount()); });

async function settle() { await act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); }); }
async function start() { await act(async () => app.render(React.createElement(App))); await settle(); }
async function click(node) { assert.ok(node); await act(async () => node.click()); await settle(); }
function promoteButton() {
  const button = [...document.querySelectorAll('#workspace-prep button')].find((node) => node.textContent.trim() === 'Promote to draft');
  assert.ok(button, 'current finding offers promotion');
  return button;
}
const writes = () => calls.filter((call) => call.method !== 'GET');

function promotedThread() {
  return {
    ...structuredClone(view.threads[0]), id: 'promoted-1', source: 'local', githubThreadId: null,
    findingId: current.finding.id, subject: 'line', agent: { state: 'idle' }, comments: [],
    reviewComment: { id: 'review-draft-1', body: 'Check token reuse before publishing.' },
  };
}

test('browser promotion posts only to the local endpoint and opens an editable returned draft', async () => {
  const thread = promotedThread();
  routes[`POST ${path}`] = () => {
    view.threads.push(thread);
    return json({ state: { ...current.userState, revision: 4, disposition: 'promoted_to_draft', promotedDraftId: thread.id }, thread, draft: thread.reviewComment });
  };
  await start();
  assert.deepEqual(writes(), []);
  await click(promoteButton());
  await settle();

  assert.equal(writes().length, 1);
  assert.equal(writes()[0].path, path);
  const payload = JSON.parse(writes()[0].body);
  assert.deepEqual(Object.keys(payload).sort(), ['expectedRevision', 'operationId']);
  assert.equal(payload.expectedRevision, 3);
  assert.match(payload.operationId, /^[0-9a-f-]{36}$/i);
  assert.equal(document.getElementById('workspace-files').hidden, false);
  const editor = document.querySelector('.desktop-conversation [aria-label="Review comment draft"]');
  assert.ok(editor, 'existing review comment editor is open');
  assert.equal(editor.value, thread.reviewComment.body);
  await act(async () => {
    Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value').set.call(editor, 'Edited before saving');
    editor.dispatchEvent(new window.Event('input', { bubbles: true }));
  });
  await settle();
  assert.equal(editor.value, 'Edited before saving');
  assert.equal(writes().length, 1, 'editing does not publish or save implicitly');
});

test('uncertain promotion retry retains its original revision and operation ID after a refresh', async () => {
  let attempts = 0;
  routes[`POST ${path}`] = () => {
    attempts++;
    if (attempts === 1) {
      current = { ...current, userState: { ...current.userState, revision: 4 } };
      throw new TypeError('connection closed');
    }
    return json({ error: 'conflict' }, 409);
  };
  await start();
  await click(promoteButton());
  await settle();
  assert.match(document.body.textContent, /Promotion outcome uncertain/);
  assert.equal(current.userState.revision, 4);
  await click(promoteButton());
  await settle();
  assert.equal(attempts, 2);
  assert.equal(writes().length, 2);
  assert.deepEqual(JSON.parse(writes()[1].body), JSON.parse(writes()[0].body), 'retry preserves both original revision and operation ID');
  assert.equal(writes()[1].path, path);
});

test('HTTP 408 after a committed promotion replays the original operation', async () => {
  let committed;
  routes[`POST ${path}`] = (_url, init) => {
    const input = JSON.parse(init.body);
    if (!committed) {
      const thread = promotedThread();
      committed = { input, thread };
      current = { ...current, userState: { ...current.userState, revision: 4, promotedDraftId: thread.reviewComment.id } };
      return json({ error: 'timeout' }, 408);
    }
    assert.deepEqual(input, committed.input);
    // Make the committed draft visible once the lagging session read catches up.
    const { thread } = committed;
    view.threads.push(thread);
    return json({ state: current.userState, thread, draft: thread.reviewComment });
  };
  await start();
  await click(promoteButton());
  await settle();
  assert.match(document.body.textContent, /Promotion outcome uncertain/);
  assert.equal(current.userState.revision, 4);
  await click(promoteButton());
  await settle();
  assert.equal(writes().length, 2);
  assert.deepEqual(JSON.parse(writes()[1].body), JSON.parse(writes()[0].body));
  assert.equal(view.threads.filter((candidate) => candidate.id === committed.thread.id).length, 1);
  assert.equal(document.querySelector('.desktop-conversation [aria-label="Review comment draft"]').value, committed.thread.reviewComment.body);
});

test('paginated history is isolated from repeated visibility and session refreshes', async () => {
  const historical = Array.from({ length: 3 }, (_, index) => ({
    ...structuredClone(findings[1]), label: 'historical',
    finding: { ...structuredClone(findings[1].finding), id: `old-${index}`, title: `Old finding ${index}` },
  }));
  routes['GET /api/v1/analysis/findings'] = (url) => {
    if (url.searchParams.get('includeHistorical') !== 'true') return json(page([current]));
    const cursor = Number(url.searchParams.get('cursor') ?? 0);
    return json({ ...page([historical[cursor]]), hasMore: cursor < 2, nextCursor: cursor < 2 ? String(cursor + 1) : null });
  };
  await start();
  const browser = () => document.querySelector('[aria-label="Browse findings"]');
  const clickLabel = async (label) => click([...browser().querySelectorAll('button')].find(button => button.textContent.trim() === label));
  await clickLabel('Historical');
  await settle();
  assert.ok(browser().textContent.includes('Old finding 2'));
  const historyCalls = () => calls.filter((call) => call.path === '/api/v1/analysis/findings' && call.search.includes('includeHistorical=true'));
  assert.equal(historyCalls().length, 3);
  await clickLabel('Open');
  for (let index = 0; index < 3; index++) {
    await act(async () => document.dispatchEvent(new window.Event('visibilitychange')));
    await act(async () => FakeEventSource.latest.emit('analysis.findings.upserted'));
    await settle();
    assert.ok(browser().textContent.includes(current.finding.title));
  }
  assert.equal(historyCalls().length, 3, 'routine refreshes never request history again');
  assert.ok(calls.filter((call) => call.path === '/api/v1/analysis/findings' && !call.search.includes('includeHistorical=true')).length >= 7);
});

test('current refresh completes while historical pagination is pending; stale history is never current', async () => {
  let releaseHistory;
  const pendingHistory = new Promise((resolve) => { releaseHistory = resolve; });
  routes['GET /api/v1/analysis/findings'] = (url) => {
    if (url.searchParams.get('includeHistorical') !== 'true') return json(page([current]));
    if (url.searchParams.has('cursor')) return pendingHistory;
    return json({ ...page([]), hasMore: true, nextCursor: 'next' });
  };
  await start();
  const browser = () => document.querySelector('[aria-label="Browse findings"]');
  const clickLabel = async (label) => click([...browser().querySelectorAll('button')].find(button => button.textContent.trim() === label));
  await clickLabel('Historical');
  await settle();
  await clickLabel('Open');
  current = { ...current, finding: { ...current.finding, title: 'Fresh current finding' } };
  await act(async () => document.dispatchEvent(new window.Event('visibilitychange')));
  await settle();
  assert.match(browser().textContent, /Fresh current finding/);
  await act(async () => releaseHistory(json(page([{ ...structuredClone(findings[1]), label: 'historical' }]))));
  await settle();
  assert.match(browser().textContent, /Fresh current finding/);
  assert.doesNotMatch(browser().textContent, /Loading findings/);
});

test('historical load failure is not shown as empty history and can be retried', async () => {
  let historicalAttempts = 0;
  const historical = { ...structuredClone(findings[1]), label: 'historical' };
  routes['GET /api/v1/analysis/findings'] = (url) => {
    if (url.searchParams.get('includeHistorical') !== 'true') return json(page([current]));
    if (++historicalAttempts <= 2) return json({ error: 'unavailable', message: 'Temporary read failure' }, 503);
    return json(page([current, historical]));
  };
  const browser = () => document.querySelector('[aria-label="Browse findings"]');
  const clickLabel = async (label) => click([...browser().querySelectorAll('button')].find(button => button.textContent.trim() === label));
  await start();
  assert.equal(historicalAttempts, 0);
  await clickLabel('Historical');
  await settle();
  assert.match(browser().textContent, /Could not load findings/);
  assert.doesNotMatch(browser().textContent, /No historical findings/);
  await clickLabel('Open');
  await settle();
  assert.ok(browser().textContent.includes(current.finding.title), 'retained current finding remains visible after history fails');
  await clickLabel('Historical');
  await settle();
  await clickLabel('Retry finding load');
  await settle();
  assert.equal(historicalAttempts, 3);
  assert.ok(browser().textContent.includes(historical.finding.title));
  assert.doesNotMatch(browser().textContent, /Could not load findings/);
});
