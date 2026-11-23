// Promotion from the mounted review stays local and opens the existing draft editor.
import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
import { findings, page, queueView, sessionView } from '../test/fixtures.mjs';

installDom();
const { mount, unmount, flushSync, tick } = await import('svelte');
const { default: App } = await import('./App.svelte');
const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
const path = `/api/v1/analysis/findings/${findings[0].finding.id}/promote`;
let app;
let calls;
let routes;
let view;
let current;

class FakeEventSource {
  addEventListener() {}
  close() {}
}

beforeEach(() => {
  app = null;
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
    calls.push({ method, path: url.pathname, body: init.body });
    const handler = routes[`${method} ${url.pathname}`];
    return handler ? handler(url, init) : json({ error: 'not_found', message: `no route ${method} ${url.pathname}` }, 404);
  };
  document.body.innerHTML = '';
  document.title = 'Aside';
});

afterEach(() => { if (app) unmount(app); });

async function settle() {
  for (let i = 0; i < 10; i++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await tick();
  }
  flushSync();
}
async function start() { app = mount(App, { target: document.body }); await settle(); }
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
  promoteButton().click();
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
  editor.value = 'Edited before saving';
  editor.dispatchEvent(new window.Event('input', { bubbles: true }));
  await tick();
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
  promoteButton().click();
  await settle();
  assert.match(document.body.textContent, /Promotion outcome uncertain/);
  assert.equal(current.userState.revision, 4);
  promoteButton().click();
  await settle();
  assert.equal(attempts, 2);
  assert.equal(writes().length, 2);
  assert.deepEqual(JSON.parse(writes()[1].body), JSON.parse(writes()[0].body), 'retry preserves both original revision and operation ID');
  assert.equal(writes()[1].path, path);
});

test('historical load failure is not shown as empty history and can be retried', async () => {
  let historicalAttempts = 0;
  const historical = { ...structuredClone(findings[1]), label: 'historical' };
  routes['GET /api/v1/analysis/findings'] = (url) => {
    if (url.searchParams.get('includeHistorical') !== 'true') return json(page([current]));
    if (++historicalAttempts === 1) return json({ error: 'unavailable', message: 'Temporary read failure' }, 503);
    return json(page([current, historical]));
  };
  const browser = () => document.querySelector('[aria-label="Browse findings"]');
  const click = (label) => [...browser().querySelectorAll('button')].find((button) => button.textContent.trim() === label).click();
  await start();
  assert.equal(historicalAttempts, 0);
  click('Historical');
  await settle();
  assert.match(browser().textContent, /Could not load findings/);
  assert.doesNotMatch(browser().textContent, /No historical findings/);
  click('Retry finding load');
  await settle();
  assert.equal(historicalAttempts, 2);
  assert.ok(browser().textContent.includes(historical.finding.title));
  assert.doesNotMatch(browser().textContent, /Could not load findings/);
});
