import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';
import { createRequire } from 'node:module';
import { installDom } from '../test/dom.mjs';
import { sessionView, queueView, findings, page } from '../test/fixtures.mjs';

installDom();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const require = createRequire(import.meta.url);
const React = require('react');
const { act } = React;
const { createRoot } = require('react-dom/client');
const { createServer } = await import('vite');
const vite = await createServer({ server: { middlewareMode: true, hmr: false }, appType: 'custom' });
const { default: App } = await vite.ssrLoadModule('/src/App.tsx');
const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
let root, view, routes, calls, sources;
class FakeEventSource {
  constructor() { this.listeners = new Map(); sources.push(this); }
  addEventListener(name, listener) { this.listeners.set(name, listener); }
  emit(name) { this.listeners.get(name)?.(); }
  close() {}
}
const button = (name) => [...document.querySelectorAll('button')].find((entry) => entry.textContent.trim() === name || entry.textContent.trim().startsWith(name + ' ') || entry.getAttribute('aria-label') === name);
const viewButton = (name) => document.querySelector(`[aria-controls="workspace-${name}"]`);
async function settle() { await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); }); }
async function click(element) { assert.ok(element); await act(async () => element.click()); await settle(); }
const type = async (node, value) => { assert.ok(node); await act(async () => { Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value').set.call(node, value); node.dispatchEvent(new window.Event('input', { bubbles: true })); }); };
beforeEach(() => {
  document.body.innerHTML = ''; document.title = 'Aside';
  root = createRoot(document.body); view = sessionView(); calls = []; sources = [];
  routes = { 'GET /api/v1/session': () => json(view), 'GET /api/v1/analysis/queue': () => json(queueView()), 'GET /api/v1/analysis/findings': () => json(page(findings)) };
  globalThis.EventSource = FakeEventSource;
  globalThis.fetch = async (input, init = {}) => {
    const url = new URL(String(input), 'http://127.0.0.1');
    const method = init.method ?? 'GET';
    calls.push({ method, path: url.pathname, body: init.body });
    return (routes[`${method} ${url.pathname}`] ?? (() => json({ error: 'not_found' }, 404)))(url, init);
  };
});
afterEach(async () => { await act(async () => root.unmount()); });
test.after(async () => vite.close());
async function start() { await act(async () => root.render(React.createElement(App))); await settle(); }

test('starts in prep; tabs preserve file, selected thread and title while loading insights independently', async () => {
  await start();
  assert.equal(document.title, '#7: Tidy auth · o/n · Aside');
  assert.equal(document.getElementById('workspace-prep').hidden, false);
  await click(button('Start reviewing'));
  assert.equal(document.getElementById('workspace-files').hidden, false);
  assert.ok(document.querySelector('[aria-label="File review"]'));
  await click(viewButton('feedback'));
  assert.equal(document.getElementById('workspace-feedback').hidden, false);
  await click(viewButton('files'));
  assert.equal(document.getElementById('workspace-files').hidden, false);
  assert.deepEqual(calls.filter((entry) => entry.method !== 'GET'), []);
});

test('unsent line draft survives file and tab changes and a changed digest without reanchoring', async () => {
  await start(); await click(button('Start reviewing'));
  const line = document.querySelector('.diff-draft');
  assert.ok(line, 'a changed line accepts a private draft');
  await click(line);
  await type(document.getElementById('draft'), 'Keep this unsent text');
  const otherLine = document.querySelectorAll('.diff-draft')[1];
  assert.ok(otherLine);
  await click(otherLine);
  assert.equal(document.getElementById('draft').value, '');
  await type(document.getElementById('draft'), 'A separate line draft');
  await click(line);
  assert.equal(document.getElementById('draft').value, 'Keep this unsent text');
  await click(otherLine);
  assert.equal(document.getElementById('draft').value, 'A separate line draft');
  await click(line);
  await click(viewButton('conversations')); await click(viewButton('files'));
  assert.equal(document.getElementById('draft').value, 'Keep this unsent text');
  view = { ...view, sequence: view.sequence + 1, files: view.files.map((entry, index) => index === 0 ? { ...entry, diffDigest: 'changed-digest' } : entry) };
  await act(async () => sources[0].emit('resync')); await settle();
  assert.match(document.body.textContent, /Unsent draft for .* is anchored to an earlier version/);
  assert.equal(document.querySelector('#draft'), null);
  assert.equal(calls.filter((entry) => entry.method !== 'GET').length, 0);
});

test('promotion uses one local operation and opens editable draft without publishing', async () => {
  const item = structuredClone(findings[0]);
  item.disposition = 'untouched'; item.userState = { revision: 3, disposition: 'untouched', snoozedUntil: null, promotedDraftId: null }; item.canPromote = true;
  routes['GET /api/v1/analysis/findings'] = () => json(page([item]));
  const thread = { ...structuredClone(view.threads[0]), id: 'promoted-1', source: 'local', githubThreadId: null, findingId: item.finding.id, subject: 'line', agent: { state: 'idle' }, comments: [], reviewComment: { id: 'draft-1', body: 'Check token reuse' } };
  const path = `/api/v1/analysis/findings/${item.finding.id}/promote`;
  routes[`POST ${path}`] = () => { view = { ...view, sequence: view.sequence + 1, threads: [...view.threads, thread] }; return json({ thread, draft: thread.reviewComment }); };
  await start();
  await click(button('Promote to draft'));
  assert.equal(calls.filter((entry) => entry.method !== 'GET').length, 1);
  assert.equal(calls.find((entry) => entry.method !== 'GET').path, path);
  assert.equal(document.getElementById('workspace-files').hidden, false);
  assert.equal(document.querySelector('.desktop-conversation [aria-label="Review comment draft"]')?.value, 'Check token reuse');
});

test('stale action refresh cannot overwrite a newer event snapshot', async () => {
  await start(); await click(button('Start reviewing'));
  let resolve;
  routes[`POST /api/v1/files/${view.files[0].id}/reviewed`] = () => new Promise((done) => { resolve = done; });
  const checkbox = document.querySelector('#file-review input[type="checkbox"]');
  await act(async () => checkbox.click());
  assert.ok(resolve, 'review action is in flight');
  view = { ...view, sequence: view.sequence + 2, pr: { ...view.pr, title: 'Newer review' } };
  await act(async () => sources[0].emit('resync')); await settle();
  resolve(json({})); await settle();
  assert.match(document.title, /Newer review/);
});

test('a replacement session disables the previous workspace and emits no mutation', async () => {
  await start();
  await click(viewButton('finish'));
  view = { ...view, sessionId: 'other-session', sequence: view.sequence + 1 };
  await act(async () => sources[0].emit('resync'));
  await settle();
  assert.match(document.querySelector('[role=alert]').textContent, /review session changed/i);
  assert.equal(document.querySelector('#workspace-finish'), null);
  assert.equal(document.querySelector('#workspace-files'), null);
  assert.deepEqual(calls.filter((entry) => entry.method !== 'GET'), []);
});
