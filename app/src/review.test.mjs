// Review screen with progressive insights (#38), mounted in jsdom.
import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
import { evidence, findings, page, queueView, sessionView, source } from '../test/fixtures.mjs';

installDom();
const { mount, unmount, flushSync, tick } = await import('svelte');
const { default: App } = await import('./App.svelte');

/** A promise the test resolves when it chooses: "analysis still pending". */
function deferred() {
  let resolve;
  const promise = new Promise((r) => { resolve = r; });
  return { promise, resolve };
}

const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

let calls;
let routes;
let sources;
let app;

class FakeEventSource {
  constructor(url) {
    this.url = url;
    this.listeners = {};
    sources.push(this);
  }
  addEventListener(type, fn) { (this.listeners[type] ??= []).push(fn); }
  emit(type) { for (const fn of this.listeners[type] ?? []) fn(new window.Event(type)); }
  close() {}
}

beforeEach(() => {
  calls = [];
  sources = [];
  routes = {};
  globalThis.EventSource = FakeEventSource;
  globalThis.fetch = async (input, init = {}) => {
    const url = new URL(String(input), 'http://127.0.0.1');
    const method = init.method ?? 'GET';
    calls.push({ method, path: url.pathname, search: url.search, body: init.body });
    const handler = routes[`${method} ${url.pathname}`];
    if (!handler) return json({ error: 'not_found', message: `no route ${method} ${url.pathname}` }, 404);
    return handler(url, init);
  };
  document.body.innerHTML = '';
  app = null;
});

afterEach(() => {
  if (app) unmount(app);
});

async function start() {
  app = mount(App, { target: document.body });
  await settle();
}

async function settle() {
  for (let i = 0; i < 10; i++) {
    await new Promise((r) => setTimeout(r, 0));
    await tick();
  }
  flushSync();
}

const text = () => document.body.textContent.replace(/\s+/g, ' ');
const byLabel = (label) => document.querySelector(`[aria-label="${label}"]`);
const button = (name) => [...document.querySelectorAll('button')].find((b) => b.textContent.replace(/\s+/g, ' ').trim() === name || b.getAttribute('aria-label') === name);
const mutations = () => calls.filter((c) => c.method !== 'GET');

test('first paint renders the full review while analysis is still pending, then updates in place', async () => {
  const queue = deferred();
  const findingPage = deferred();
  routes['GET /api/v1/session'] = () => json(sessionView());
  routes['GET /api/v1/analysis/queue'] = () => queue.promise;
  routes['GET /api/v1/analysis/findings'] = () => findingPage.promise;
  await start();

  // Everything needed to review is on screen with analysis unresolved.
  assert.match(text(), /Tidy auth/);
  assert.match(text(), /verify_token\(\);/);
  assert.ok(document.querySelector('nav[aria-label="Changed files"]'));
  const viewed = [...document.querySelectorAll('input[type=checkbox]')].find((c) => c.parentElement.textContent.includes('Viewed'));
  assert.equal(viewed.disabled, false);
  assert.match(document.querySelector('[data-testid=analysis-status]').textContent, /running/);
  assert.match(document.querySelector('[data-testid=findings-pending]').textContent, /Loading findings/);
  assert.equal(document.querySelector('[data-testid=file-indicators]'), null);

  // The reviewer is reading: focus a diff control. Results must not steal it.
  const draftButton = [...document.querySelectorAll('button[title="Draft a comment on this line"]')][0];
  draftButton.focus();
  assert.equal(document.activeElement, draftButton);

  queue.resolve(json(queueView()));
  findingPage.resolve(json(page(findings)));
  await settle();

  assert.equal(document.activeElement, draftButton, 'insights must not move focus');
  assert.match(text(), /verify_token\(\);/, 'diff stays rendered');
  const indicators = document.querySelector('[data-testid=file-indicators]');
  assert.ok(indicators, 'file-tree indicators appear when results arrive');
  const authButton = byLabel('Review src/auth.rs. High attention, 1 unresolved GitHub thread');
  assert.ok(authButton, 'indicator meaning is in the accessible name, not only colour');
  assert.ok(document.querySelector('article[aria-label="Measured fact: Changes 2 lines in an auth path"]'));
});

test('analysis never marks a file viewed or reviewed', async () => {
  routes['GET /api/v1/session'] = () => json(sessionView({ overall: 'complete', sources: [source('aside.diff-facts', 'has_results')] }));
  routes['GET /api/v1/analysis/queue'] = () => json(queueView());
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
  await start();
  // Analysis progress events arrive and are applied.
  for (const event of ['analysis.run.succeeded', 'analysis.findings.upserted']) sources[0].emit(event);
  await settle();
  assert.ok(calls.filter((c) => c.path === '/api/v1/analysis/queue').length >= 2);
  // Even in suggested order, with high-attention results loaded.
  button('Suggested order').click();
  await settle();
  assert.deepEqual(mutations(), [], 'no write of any kind was made');
  const viewed = [...document.querySelectorAll('input[type=checkbox]')].find((c) => c.parentElement.textContent.includes('Viewed'));
  assert.equal(viewed.checked, false);
  assert.match(text(), /0 of 1 in-scope files reviewed/);
});

test('partial coverage with no findings is never presented as all clear', async () => {
  const partial = [
    source('github.review_threads', 'has_results', { mode: 'github_import', completeness: 'complete' }),
    source('aside.diff-facts', 'incomplete', { status: 'partial', completeness: 'partial', limitations: ['Skipped 3 files over the size limit'] }),
  ];
  routes['GET /api/v1/session'] = () => json(sessionView({ overall: 'incomplete', sources: partial }));
  routes['GET /api/v1/analysis/queue'] = () => json(queueView({ coverage: 'incomplete', verdict: { kind: 'empty_incomplete' } }));
  routes['GET /api/v1/analysis/findings'] = () => json(page([], { complete: false, sources: partial }));
  await start();

  const status = document.querySelector('[data-testid=analysis-status]');
  assert.equal(status.dataset.coverage, 'incomplete');
  assert.match(status.textContent, /1 of 2 sources complete · 1 partial/);
  assert.match(text(), /Skipped 3 files over the size limit/);
  const empty = document.querySelector('[data-testid=findings-empty]').textContent;
  assert.match(empty, /coverage is incomplete\. This is not an all-clear\./);
  assert.doesNotMatch(text(), /Every analysis source completed|Nothing remains and every source completed/);
  button('Suggested order').click();
  await settle();
  assert.match(document.querySelector('[data-testid=queue-verdict]').textContent, /not an all-clear/);
});

test('measured facts, suggestions and external feedback are distinct in text and accessible name', async () => {
  routes['GET /api/v1/session'] = () => json(sessionView({ overall: 'complete', sources: [source('aside.diff-facts', 'has_results')] }));
  routes['GET /api/v1/analysis/queue'] = () => json(queueView());
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
  routes['GET /api/v1/analysis/findings/fnd-measured/evidence'] = () => json(page(evidence));
  await start();

  const cards = [...document.querySelectorAll('article[data-insight-class]')];
  assert.deepEqual(cards.map((c) => c.dataset.insightClass), ['measured', 'suggestion', 'external']);
  assert.deepEqual(cards.map((c) => c.getAttribute('aria-label').split(':')[0]), ['Measured fact', 'Suggestion · attention hint', 'External feedback']);
  assert.equal(new Set(cards.map((c) => c.className)).size, 3, 'visual treatment differs, not only the label');
  // Native GitHub threads stay conversational and say where they came from.
  assert.match(document.getElementById('thread-t1').textContent, /External feedback · GitHub/);
  assert.match(text(), /review by carol|carol/);
  // Never an invented confidence percentage or score.
  assert.doesNotMatch(text(), /%|0\.9[37]/);

  // Evidence card: cited lines, rule and version, provider.
  const show = button('Show evidence for Changes 2 lines in an auth path');
  assert.equal(show.getAttribute('aria-expanded'), 'false');
  show.click();
  await settle();
  const hide = button('Hide evidence for Changes 2 lines in an auth path');
  assert.equal(hide, show);
  assert.equal(show.getAttribute('aria-expanded'), 'true');
  const panel = document.getElementById(show.getAttribute('aria-controls'));
  assert.match(panel.textContent, /aside\.diff-facts/);
  assert.match(panel.textContent, /auth_path v2/);
  assert.match(panel.textContent, /new line 2 of src\/auth\.rs/);
  assert.match(panel.textContent, /2 lines · 2 of 40/);
  assert.match(panel.textContent, /new lines 1–2 of src\/auth\.rs/);
  assert.match(panel.textContent, /Whitespace-only changes are counted/);

  // Queue reasons render the #30 reasons with what they cite.
  const why = byLabel('Reasons for src/auth.rs').textContent.replace(/\s+/g, ' ');
  assert.match(why, /Unresolved GitHub thread — thread by bob on line 2/);
  assert.match(why, /High attention from a deterministic rule — finding “Changes 2 lines in an auth path” · provider aside\.diff-facts · rule auth_path/);
});

test('suggested order filters, reveals ignored files and re-including does not rerun analysis', async () => {
  let ruled = false;
  routes['GET /api/v1/session'] = () => json(sessionView({ overall: 'complete', sources: [source('aside.diff-facts', 'has_results')] }));
  routes['GET /api/v1/analysis/queue'] = () => json(queueView({ genIgnored: !ruled }));
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
  routes['POST /api/v1/repositories/current/exclusions'] = () => { ruled = true; return json({}); };
  await start();

  button('Suggested order').click();
  await settle();
  const list = () => [...document.querySelectorAll('ol[aria-label="Suggested review order"] button')].map((b) => b.querySelector('.font-mono').textContent);
  assert.deepEqual(list(), ['src/auth.rs'], 'ignored files are outside the normal queue');
  const reveal = [...document.querySelectorAll('input[type=checkbox]')].find((c) => c.parentElement.textContent.includes('Reveal 1 ignored file'));
  assert.ok(reveal, 'ignored files are revealable, with a count');
  reveal.click();
  await settle();
  assert.deepEqual(list(), ['src/auth.rs', 'gen/out.rs']);
  const select = document.querySelector('select');
  select.value = 'high';
  select.dispatchEvent(new window.Event('change'));
  await settle();
  assert.deepEqual(list(), ['src/auth.rs']);
  select.value = 'ignored';
  select.dispatchEvent(new window.Event('change'));
  await settle();
  assert.deepEqual(list(), ['gen/out.rs']);

  // Open the ignored file and include it.
  document.querySelector('ol[aria-label="Suggested review order"] button').click();
  await settle();
  const before = calls.filter((c) => c.path === '/api/v1/analysis/queue').length;
  button('Include in review gen/out.rs').click();
  await settle();
  const writes = mutations();
  assert.equal(writes.length, 1);
  assert.equal(writes[0].path, '/api/v1/repositories/current/exclusions');
  assert.deepEqual(JSON.parse(writes[0].body), { action: 'include', kind: 'file', expression: 'gen/out.rs' });
  assert.ok(calls.filter((c) => c.path === '/api/v1/analysis/queue').length > before, 'queue re-read');
  assert.equal(calls.filter((c) => c.path.startsWith('/api/v1/analysis/runs')).length, 0, 'no analysis rerun');
});

test('every new insight control is a native, named, keyboard-focusable element with visible focus', async () => {
  routes['GET /api/v1/session'] = () => json(sessionView({ overall: 'complete', sources: [source('aside.diff-facts', 'has_results')] }));
  routes['GET /api/v1/analysis/queue'] = () => json(queueView());
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
  await start();
  button('Suggested order').click();
  await settle();

  const regions = [
    document.querySelector('[aria-labelledby=insights-heading]'),
    document.querySelector('[aria-label="Analysis status"]'),
    document.querySelector('[aria-label="File order"]'),
    document.querySelector('ol[aria-label="Suggested review order"]').parentElement,
  ];
  let checked = 0;
  for (const region of regions) {
    assert.ok(region);
    // No click handlers on non-interactive elements: interactive things are native.
    for (const el of region.querySelectorAll('[role=button], [onclick], div[tabindex], span[tabindex]')) {
      assert.fail(`non-native control: ${el.outerHTML}`);
    }
    for (const el of region.querySelectorAll('button, select, input, summary, a[href]')) {
      checked++;
      assert.notEqual(el.getAttribute('tabindex'), '-1', el.outerHTML);
      const name = (el.getAttribute('aria-label') ?? el.closest('label')?.textContent ?? el.textContent).trim();
      assert.ok(name.length > 0, `unnamed control: ${el.outerHTML}`);
      if (el.tagName !== 'INPUT') assert.match(el.className, /focus-visible:outline/, `no visible focus: ${el.outerHTML}`);
      el.focus();
      assert.equal(document.activeElement, el, `not focusable: ${el.outerHTML}`);
    }
  }
  assert.ok(checked >= 10, `checked ${checked} controls`);

  // Toggles expose state to assistive tech and update it.
  const measured = button('Measured');
  assert.equal(measured.getAttribute('aria-pressed'), 'false');
  measured.click();
  await settle();
  assert.equal(measured.getAttribute('aria-pressed'), 'true');
  assert.deepEqual([...document.querySelectorAll('article[data-insight-class]')].map((c) => c.dataset.insightClass), ['measured']);
  // Status is announced politely, not as an interruption.
  assert.equal(document.querySelector('[data-testid=analysis-status]').getAttribute('aria-live'), 'polite');
});

test('stale cursors during a live run restart pagination instead of splicing pages', async () => {
  const { fetchAllPages, ResultsChangedError } = await import('./api.ts');
  let served = 0;
  routes['GET /api/v1/analysis/findings'] = (url) => {
    served++;
    if (url.searchParams.get('cursor') === 'c1' && served === 2) {
      return json({ error: 'cursor_stale', message: 'resync', details: { retryable: true } }, 409);
    }
    if (!url.searchParams.get('cursor')) return json({ ...page([findings[0]]), hasMore: true, nextCursor: 'c1' });
    return json(page([findings[1]]));
  };
  const { items } = await fetchAllPages('/api/v1/analysis/findings?includeIgnored=true');
  assert.deepEqual(items.map((i) => i.finding.id), ['fnd-measured', 'fnd-hint'], 'restarted cleanly, no duplicates');
  assert.equal(served, 4);

  routes['GET /api/v1/analysis/findings'] = (url) => url.searchParams.get('cursor')
    ? json({ error: 'cursor_stale', message: 'resync' }, 409)
    : json({ ...page([findings[0]]), hasMore: true, nextCursor: 'c1' });
  await assert.rejects(fetchAllPages('/api/v1/analysis/findings'), ResultsChangedError);
});
