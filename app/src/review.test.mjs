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
const workspaceView = (id) => document.getElementById(`workspace-${id}`);
const viewButton = (id) => document.querySelector(`button[aria-controls="workspace-${id}"]`);

function workspaceRoutes(view = sessionView) {
  routes['GET /api/v1/session'] = () => json(view());
  routes['GET /api/v1/analysis/queue'] = () => json(queueView());
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
}

test('review prep is the initial view while analysis is pending, then updates in place', async () => {
  const queue = deferred();
  const findingPage = deferred();
  routes['GET /api/v1/session'] = () => json(sessionView());
  routes['GET /api/v1/analysis/queue'] = () => queue.promise;
  routes['GET /api/v1/analysis/findings'] = () => findingPage.promise;
  await start();

  // Scope, focus areas, and progress are visible before opening a diff.
  assert.match(text(), /Tidy auth/);
  assert.equal(workspaceView('prep').hidden, false);
  assert.equal(workspaceView('files').hidden, true);
  assert.match(workspaceView('prep').textContent, /Start with the important bits/);
  assert.match(workspaceView('prep').textContent, /Suggested places to look/);
  assert.match(workspaceView('prep').textContent, /Loading suggested order/);
  assert.match(workspaceView('files').textContent, /verify_token\(\);/);
  assert.ok(document.querySelector('nav[aria-label="Changed files"]'));
  const viewed = [...document.querySelectorAll('input[type=checkbox]')].find((c) => c.parentElement.textContent.includes('Viewed'));
  assert.equal(viewed.disabled, false);
  assert.match(document.querySelector('[data-testid=analysis-status]').textContent, /running/);
  assert.match(document.querySelector('[data-testid=findings-pending]').textContent, /Loading findings/);
  assert.equal(document.querySelector('[data-testid=file-indicators]'), null);
  assert.equal(document.querySelector('.workspace-context [aria-labelledby=insights-heading]'), null, 'no right insights panel');

  // The reviewer is reading prep: results must not steal focus.
  const prepHeading = document.getElementById('review-prep-heading');
  prepHeading.focus();
  assert.equal(document.activeElement, prepHeading);

  queue.resolve(json(queueView()));
  findingPage.resolve(json(page(findings)));
  await settle();

  assert.equal(document.activeElement, prepHeading, 'insights must not move focus');
  assert.equal(workspaceView('prep').hidden, false, 'prep remains the active view');
  assert.match(text(), /verify_token\(\);/, 'diff remains available in the workspace');
  const indicators = document.querySelector('[data-testid=file-indicators]');
  assert.ok(indicators, 'file-tree indicators appear when results arrive');
  const authButton = byLabel('Review src/auth.rs. High attention, 1 unresolved GitHub thread');
  assert.ok(authButton, 'indicator meaning is in the accessible name, not only colour');
  assert.ok(document.querySelector('article[aria-label="Measured fact: Changes 2 lines in an auth path"]'));
});

test('file band, inline adjacency, markers and reveal are read-only', async () => {
  const whole = structuredClone(findings[0]);
  whole.finding.id = 'whole';
  whole.finding.anchor = { ...whole.finding.anchor, kind: 'file', line: null, side: null };
  const range = structuredClone(findings[1]);
  range.finding.id = 'range';
  range.finding.anchor = { ...range.finding.anchor, kind: 'diff_range', startLine: 1, line: 2 };
  workspaceRoutes();
  routes['GET /api/v1/analysis/findings'] = () => json(page([whole, range]));
  await start();
  const diff = document.getElementById('file-review');
  assert.equal(diff.querySelector('.callout-band article').id, 'callout-whole');
  const row = [...diff.querySelectorAll('.diff-row')].find((el) => el.textContent.includes('verify_token'));
  assert.equal(row.nextElementSibling.querySelector('article').id, 'callout-range');
  assert.equal(row.querySelector('.callout-marker').tagName, 'BUTTON');
  button('Hide line callouts').click();
  await tick();
  assert.equal(diff.querySelectorAll('.inline-callouts').length, 0);
  assert.ok(row.querySelector('.callout-marker'));
  row.querySelector('.callout-marker').click();
  await tick();
  assert.equal(document.activeElement.id, 'callout-range');
  assert.equal(row.nextElementSibling.querySelector('article').id, 'callout-range');
  assert.deepEqual(mutations(), []);
});

test('context row presents old and new range endpoints together with honest marker labels', async () => {
  const view = sessionView();
  view.files[0].hunks[0].lines[1] = { ...view.files[0].hunks[0].lines[1], kind: 'context', oldLine: 2 };
  const left = structuredClone(findings[0]);
  left.finding.id = 'left-range';
  left.finding.anchor = { ...left.finding.anchor, kind: 'diff_range', side: 'LEFT', startLine: 1, line: 2 };
  const right = structuredClone(findings[1]);
  right.finding.id = 'right-range';
  right.finding.anchor = { ...right.finding.anchor, kind: 'diff_range', side: 'RIGHT', startLine: 1, line: 2 };
  workspaceRoutes(() => view);
  routes['GET /api/v1/analysis/findings'] = () => json(page([left, right]));
  await start();
  const row = [...document.querySelectorAll('.diff-row')].find((element) => element.textContent.includes('verify_token'));
  assert.match(row.querySelector('.callout-marker').getAttribute('aria-label'), /old line 2 and new line 2/);
  assert.deepEqual([...row.nextElementSibling.querySelectorAll('article')].map((card) => card.id), ['callout-left-range', 'callout-right-range']);
  assert.match(row.nextElementSibling.getAttribute('aria-label'), /old line 2 and new line 2/);
  assert.match(row.querySelector('span.w-9').className, /w-9/);
  assert.deepEqual(mutations(), []);
});

test('selected filter empty state names the filter rather than claiming the file is empty', async () => {
  workspaceRoutes();
  routes['GET /api/v1/analysis/findings'] = () => json(page([findings[1]]));
  await start();
  assert.equal(document.querySelectorAll('article[data-insight-class="suggestion"]').length, 1);
  button('Measured').click();
  await tick();
  assert.match(document.querySelector('[data-testid=findings-empty]').textContent, /this file matching “Measured”/);
  assert.doesNotMatch(document.querySelector('[data-testid=findings-empty]').textContent, /No findings for this file\./);
  button('Suggestions').click();
  await tick();
  assert.equal(document.querySelector('[data-testid=findings-empty]'), null);
  assert.deepEqual(mutations(), []);
});

test('snapshot mismatch holds old findings off lines and fetch keeps focused draft', async () => {
  const view = sessionView();
  workspaceRoutes(() => view);
  await start();
  document.querySelector('[title="Draft a comment on this line"]').click();
  await tick();
  const draft = document.getElementById('draft');
  draft.value = 'unfinished';
  draft.dispatchEvent(new window.Event('input', { bubbles: true }));
  draft.focus();
  sources[0].emit('analysis.findings.upserted');
  await settle();
  assert.equal(document.activeElement, draft);
  assert.equal(draft.value, 'unfinished');
  view.analysis.snapshotId = 'snap-2';
  sources[0].emit('pr.refreshed');
  await settle();
  assert.equal(document.querySelectorAll('.callout-marker').length, 0);
  assert.equal(document.querySelectorAll('.inline-callouts').length, 0);
  assert.match(document.querySelector('.callout-band article')?.textContent ?? '', /snapshot mismatch/);
  assert.deepEqual(mutations(), []);
});

test('prep explains and deep-links an attention finding without writing', async () => {
  const queue = queueView();
  queue.queue.items[0].findings = [{ id: 'fnd-measured', current: true, attention: 'high', disposition: 'active' }];
  routes['GET /api/v1/session'] = () => json(sessionView());
  routes['GET /api/v1/analysis/queue'] = () => json(queue);
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
  await start();
  assert.match(workspaceView('prep').textContent, /Path matches the auth rule/);
  button('Review finding Changes 2 lines in an auth path in src/auth.rs').click();
  await tick();
  assert.equal(workspaceView('files').hidden, false);
  assert.equal(document.activeElement.id, 'callout-fnd-measured');
  assert.deepEqual(mutations(), []);
});

test('prep native projection opens its real thread rather than a missing callout', async () => {
  const projection = structuredClone(findings[2]);
  projection.finding.id = 'native-projection';
  projection.finding.anchor.githubThreadId = 'GH_T1';
  const queue = queueView();
  queue.queue.items[0].findings = [{ id: projection.finding.id, current: true, attention: 'high', disposition: 'active' }];
  routes['GET /api/v1/session'] = () => json(sessionView());
  routes['GET /api/v1/analysis/queue'] = () => json(queue);
  routes['GET /api/v1/analysis/findings'] = () => json(page([projection]));
  await start();
  assert.match(workspaceView('prep').textContent, /CodeRabbit said tokens may leak/);
  button('Open thread for finding CodeRabbit said tokens may leak in src/auth.rs').click();
  await tick();
  assert.equal(document.activeElement.id, 'thread-t1');
  assert.equal(document.getElementById('callout-native-projection'), null);
  assert.deepEqual(mutations(), []);
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
  assert.equal(document.querySelectorAll('.callout-marker').length, 1, 'filters do not remove gutter markers');
  // Status is announced politely, not as an interruption.
  assert.equal(document.querySelector('[data-testid=analysis-status]').getAttribute('aria-live'), 'polite');
});

test('workspace navigation keeps drafts and opens the actual file thread without making writes', async () => {
  workspaceRoutes();
  await start();
  const diff = workspaceView('files');
  const conversations = workspaceView('conversations');
  diff.querySelector('[title="Draft a comment on this line"]').click();
  await tick();
  const draft = document.getElementById('draft');
  draft.value = 'Still working on this draft';
  draft.dispatchEvent(new window.Event('input', { bubbles: true }));

  viewButton('conversations').click();
  await tick();
  assert.equal(viewButton('conversations').getAttribute('aria-pressed'), 'true');
  assert.equal(diff.hidden, true);
  assert.equal(conversations.hidden, false);
  assert.match(conversations.textContent, /Looks close\./);
  button('Open GitHub thread by bob on src/auth.rs:2').click();
  await tick();
  assert.equal(conversations.hidden, true);
  assert.equal(diff.hidden, false);
  assert.equal(document.activeElement.id, 'thread-t1');
  assert.equal(draft.value, 'Still working on this draft');
  assert.equal(diff.querySelector('.thread-marker').getAttribute('aria-pressed'), 'true');
  assert.deepEqual(mutations(), []);
});

test('adding a line draft focuses the comment field', async () => {
  workspaceRoutes();
  await start();
  viewButton('files').click();
  await tick();

  workspaceView('files').querySelector('[title="Draft a comment on this line"]').click();
  await tick();

  const comment = document.getElementById('draft');
  assert.ok(comment);
  assert.equal(document.activeElement, comment);
});

test('review prep shows real suggestions and editable file scope without leaving the view', async () => {
  const view = sessionView();
  workspaceRoutes(() => view);
  routes['POST /api/v1/repositories/current/exclusions'] = () => {
    const generated = view.fileStates[1];
    generated.scope.ignored = false;
    generated.status = 'unreviewed';
    view.progress.inScope = 2;
    view.progress.ignored = 0;
    view.progress.unreviewed = 2;
    return json({});
  };
  await start();
  viewButton('prep').click();
  await tick();
  assert.match(workspaceView('prep').textContent, /Start with the important bits/);
  assert.ok(button('Review suggested file src/auth.rs'));
  assert.equal(button('Review suggested file gen/out.rs'), undefined);
  button('Hide ignored').click();
  await tick();
  assert.equal(button('Hide ignored').getAttribute('aria-pressed'), 'true');
  assert.equal(button('Include file gen/out.rs'), undefined);
  button('Hide ignored').click();
  await tick();
  button('Include file gen/out.rs').click();
  await settle();
  assert.equal(workspaceView('prep').hidden, false);
  assert.deepEqual([...workspaceView('prep').querySelectorAll('dd')].map((el) => el.textContent), ['2', '0', '0']);
  assert.deepEqual(mutations().map((write) => JSON.parse(write.body)), [{ action: 'include', kind: 'file', expression: 'gen/out.rs' }]);
  button('Review suggested file src/auth.rs').click();
  await tick();
  assert.equal(workspaceView('files').hidden, false);
  assert.equal(document.activeElement.id, 'file-review-heading');
  assert.match(workspaceView('files').textContent, /verify_token\(\);/);
});

test('the diff still submits a line draft, replies locally and advances after Viewed', async () => {
  const view = sessionView();
  view.fileStates[1].scope.ignored = false;
  view.fileStates[1].status = 'unreviewed';
  workspaceRoutes(() => view);
  routes['POST /api/v1/threads'] = () => json({});
  routes['POST /api/v1/threads/t1/replies'] = () => json({});
  routes['POST /api/v1/files/f-auth/reviewed'] = () => {
    view.fileStates[0].reviewStatus = 'reviewed-current';
    return json({});
  };
  await start();
  const diff = workspaceView('files');
  const fill = (id, value) => {
    const input = document.getElementById(id);
    input.value = value;
    input.dispatchEvent(new window.Event('input', { bubbles: true }));
  };
  diff.querySelectorAll('[title="Draft a comment on this line"]')[1].click();
  await tick();
  fill('draft', 'My local comment');
  await tick();
  button('Save draft').click();
  await settle();
  fill('reply-t1', 'My local reply');
  await tick();
  button('Save reply').click();
  await settle();
  diff.querySelector('input[type=checkbox]').click();
  await settle();
  assert.deepEqual(mutations().map(({ path, body }) => [path, JSON.parse(body)]), [
    ['/api/v1/threads', { fileId: 'f-auth', diffDigest: 'digest-f-auth', side: 'RIGHT', line: 2, body: 'My local comment' }],
    ['/api/v1/threads/t1/replies', { body: 'My local reply' }],
    ['/api/v1/files/f-auth/reviewed', { diffDigest: 'digest-f-auth' }],
  ]);
  assert.match(diff.querySelector('#file-review h3').textContent, /gen\/out\.rs/);
});

for (const modifier of ['metaKey', 'ctrlKey']) {
  test(`${modifier}+Enter saves a local draft without clicking Save draft`, async () => {
    workspaceRoutes();
    const pending = deferred();
    routes['POST /api/v1/threads'] = () => pending.promise;
    await start();
    document.querySelector('[title="Draft a comment on this line"]').click();
    await tick();
    const input = document.getElementById('draft');
    const press = (options = {}) => {
      const event = new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true, ...options });
      input.dispatchEvent(event);
      return event;
    };
    press({ [modifier]: true });
    assert.equal(mutations().length, 0, 'empty drafts are not saved');
    input.value = 'My keyboard draft';
    input.dispatchEvent(new window.Event('input', { bubbles: true }));
    await tick();
    assert.equal(press().defaultPrevented, false, 'plain Enter keeps newline behavior');
    press({ [modifier]: true, isComposing: true });
    press({ [modifier]: true, repeat: true });
    assert.equal(mutations().length, 0, 'composition and key repeats do not submit');
    assert.equal(press({ [modifier]: true }).defaultPrevented, true);
    press({ [modifier]: true });
    assert.equal(mutations().length, 1, 'busy saves are not duplicated');
    assert.deepEqual(JSON.parse(mutations()[0].body), {
      fileId: 'f-auth', diffDigest: 'digest-f-auth', side: 'RIGHT', line: 1, body: 'My keyboard draft',
    });
    pending.resolve(json({}));
    await settle();
    assert.equal(document.getElementById('draft'), null);
  });
}

for (const modifier of ['metaKey', 'ctrlKey']) {
  test(`${modifier}+Enter saves a local reply without clicking Save reply`, async () => {
    workspaceRoutes();
    const pending = deferred();
    routes['POST /api/v1/threads/t1/replies'] = () => pending.promise;
    await start();
    const input = document.getElementById('reply-t1');
    const press = (options = {}) => {
      const event = new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true, ...options });
      input.dispatchEvent(event);
      return event;
    };
    press({ [modifier]: true });
    assert.equal(mutations().length, 0, 'empty replies are not saved');
    input.value = 'My keyboard reply';
    input.dispatchEvent(new window.Event('input', { bubbles: true }));
    await tick();
    assert.equal(press().defaultPrevented, false, 'plain Enter keeps newline behavior');
    press({ [modifier]: true, isComposing: true });
    press({ [modifier]: true, repeat: true });
    assert.equal(mutations().length, 0, 'composition and key repeats do not submit');
    assert.equal(press({ [modifier]: true }).defaultPrevented, true);
    press({ [modifier]: true });
    assert.equal(mutations().length, 1, 'busy saves are not duplicated');
    assert.equal(mutations()[0].path, '/api/v1/threads/t1/replies');
    assert.deepEqual(JSON.parse(mutations()[0].body), { body: 'My keyboard reply' });
    pending.resolve(json({}));
    await settle();
    assert.equal(document.getElementById('reply-t1').value, '');
  });
}

test('selecting adjacent code lines creates a contextual thread anchored to the range', async () => {
  workspaceRoutes();
  routes['POST /api/v1/threads'] = () => json({});
  await start();
  const lines = [...document.querySelectorAll('#file-review [data-code-line]')];
  const range = document.createRange();
  range.setStart(lines[0].lastChild, 1);
  range.setEnd(lines[1].lastChild, lines[1].lastChild.textContent.length);
  window.getSelection().removeAllRanges();
  window.getSelection().addRange(range);
  document.querySelector('.workspace-code').dispatchEvent(new window.MouseEvent('mouseup', { bubbles: true }));
  await tick();
  assert.match(document.querySelector('label[for="draft"]').textContent, /new lines 1–2/);
  const input = document.getElementById('draft');
  input.value = 'Why is this check added?';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  await tick();
  button('Save draft').click();
  await settle();
  assert.deepEqual(JSON.parse(mutations()[0].body), {
    fileId: 'f-auth', diffDigest: 'digest-f-auth', side: 'RIGHT', line: 2,
    body: 'Why is this check added?', startLine: 1,
  });
});

test('native feedback loads all pages, searches a later reply and preserves ignored state on reveal/navigation', async () => {
  workspaceRoutes();
  const view = sessionView();
  view.fileStates[0].scope.ignored = true;
  routes['GET /api/v1/session'] = () => json(view);
  const thread = { ...view.threads[0], id: 'native-1', source: 'github', path: view.files[0].path,
    resolved: false, resolutionKnown: true, github: { nodeId: 'native-1', isResolved: false, isOutdated: false, comments: [] },
    comments: [{ ...view.threads[0].comments[0], id: 'reply-late', body: 'Unique later page reply' }] };
  routes['GET /api/v1/github-feedback/threads'] = (url) => json(url.searchParams.has('cursor')
    ? page([thread]) : { ...page(Array.from({ length: 200 }, (_, i) => ({ ...thread, id: `resolved-${i}`, resolved: true, github: { ...thread.github, isResolved: true } }))), hasMore: true, nextCursor: 'later' });
  routes['GET /api/v1/github-feedback/records'] = (url) => json(url.searchParams.has('cursor')
    ? page([{ resourceKind: 'github_check_annotation', native: { key: 'annotation-1' }, raw: { message: 'Later annotation', path: 'src/a.rs', endLine: 3, checkRunId: 42, annotationLevel: 'warning' } }])
    : { ...page(Array.from({ length: 200 }, (_, i) => ({ resourceKind: 'github_check_run', native: { key: `run-${i}` }, raw: { name: `Run ${i}`, conclusion: 'success' } }))), hasMore: true, nextCursor: 'later' });
  await start();
  viewButton('feedback').click();
  await tick();
  assert.match(workspaceView('feedback').textContent, /1 confirmed unresolved thread on ignored files/);
  assert.doesNotMatch(workspaceView('feedback').textContent, /Unique later page reply/);
  button('Reveal ignored feedback').click();
  await tick();
  const search = document.getElementById('feedback-search');
  search.value = 'Unique later page reply';
  search.dispatchEvent(new window.Event('input', { bubbles: true }));
  await tick();
  assert.match(workspaceView('feedback').textContent, /Unique later page reply/);
  assert.ok(button(`Open ${thread.path} in review`));
  button(`Open ${thread.path} in review`).click();
  await tick();
  assert.equal(workspaceView('files').hidden, false);
  assert.deepEqual(mutations(), []);
  assert.equal(view.fileStates[0].scope.ignored, true);
  assert.equal(calls.filter((call) => call.path === '/api/v1/github-feedback/threads').length, 2);
  assert.equal(calls.filter((call) => call.path === '/api/v1/github-feedback/records').length, 2);
});

test('native feedback distinguishes missing metadata and partial source coverage', async () => {
  workspaceRoutes();
  routes['GET /api/v1/github-feedback/threads'] = () => json(page([]));
  routes['GET /api/v1/github-feedback/records'] = () => json(page([{ resourceKind: 'github_status', native: { key: 's1' }, raw: { description: 'Queued' } }], {
    complete: false, sources: [source('github.commit_statuses', 'incomplete', { completeness: 'partial', limitations: ['page limit reached'] })],
  }));
  await start();
  viewButton('feedback').click();
  await tick();
  assert.match(workspaceView('feedback').textContent, /Source link unavailable/);
  assert.match(workspaceView('feedback').textContent, /freshness: unknown/);
  assert.match(workspaceView('feedback').textContent, /page limit reached/);
  assert.match(workspaceView('feedback').textContent, /Native metadata/);
  assert.deepEqual(mutations(), []);
});

test('native cards show supported links, anchors, freshness and unmappable navigation without writes', async () => {
  workspaceRoutes();
  const view = sessionView();
  view.fileStates[0].scope.ignored = true;
  routes['GET /api/v1/session'] = () => json(view);
  const original = view.threads[0];
  const thread = { ...original, source: 'github', id: 'linked-thread', path: view.files[0].path,
    resolved: false, resolutionKnown: true, headSha: 'stale-persisted-head', anchorState: 'outdated', anchorReason: 'head changed', startLine: 2, line: 5,
    github: { nodeId: 'linked-thread', url: 'https://github.com/example/thread', isOutdated: true, isResolved: false,
      subjectType: 'LINE', originalLine: 4, comments: [{ nodeId: 'native-comment', databaseId: null,
        url: 'https://github.com/example/comment', authorType: 'Bot', outdated: true, line: null, originalLine: 4, commitOid: view.pr.headSha }] },
    comments: [{ ...original.comments[0], id: 'linked-comment', githubCommentId: null, githubNodeId: 'native-comment', body: 'Linked reply' }] };
  routes['GET /api/v1/github-feedback/threads'] = () => json(page([thread, { ...thread, id: 'orphan', path: 'missing/file.rs', github: null, resolutionKnown: false }]));
  routes['GET /api/v1/github-feedback/records'] = () => json(page([
    { resourceKind: 'check_runs:oldsha', native: { key: 'old' }, currentHead: false, appSlug: 'checks-app', name: 'Old run', status: 'completed', annotationCoverage: { state: 'partial' }, headSha: 'oldsha', detailsUrl: 'https://github.com/example/run' },
    { resourceKind: 'check_runs:newsha', native: { key: 'new' }, currentHead: true, appSlug: 'checks-app', name: 'New run', headSha: 'newsha' },
    { resourceKind: 'check_annotations:oldsha', native: { key: 'annotation' }, path: 'src/file.rs', startLine: 3, endLine: 7, checkRunId: 4, annotationLevel: 'warning', message: 'Check range' },
  ]));
  await start();
  viewButton('feedback').click();
  button('Reveal ignored feedback').focus();
  assert.equal(document.activeElement, button('Reveal ignored feedback'));
  button('Reveal ignored feedback').click();
  await tick();
  const feedback = workspaceView('feedback');
  assert.match(feedback.textContent, /anchor outdated · head changed/);
  assert.match(feedback.textContent, /lines 2–5/);
  assert.match(feedback.textContent, /Native anchor: outdated · LINE · original line 4/);
  assert.match(feedback.textContent, /native commit matches current head \(not necessarily refreshed\)/);
  assert.doesNotMatch(feedback.textContent, /native commit stale-persisted-head/);
  assert.match(feedback.textContent, /Resolution unknown/);
  assert.match(feedback.textContent, /Native comment anchor: outdated · line unknown · original line 4/);
  assert.match(feedback.textContent, /\(Bot\)/);
  assert.match(feedback.textContent, /src\/file.rs:3–7/);
  assert.match(feedback.textContent, /historical head/);
  assert.match(feedback.textContent, /current head \(not necessarily refreshed\)/);
  assert.match(feedback.textContent, /historical head/);
  assert.match(feedback.textContent, /checks-app/);
  assert.match(feedback.textContent, /Annotation coverage/);
  assert.equal(feedback.querySelectorAll('section[aria-label="Check runs"]').length, 1);
  assert.equal(feedback.querySelector('a[href="https://github.com/example/thread"]').textContent, 'Open thread on GitHub');
  assert.equal(feedback.querySelector('a[href="https://github.com/example/comment"]').textContent, 'Open comment on GitHub');
  assert.match(feedback.textContent, /No matching current file/);
  assert.match(feedback.textContent, /Thread source link unavailable/);
  assert.equal(button('Open missing/file.rs in review'), undefined);
  assert.deepEqual(mutations(), []);
  assert.equal(view.fileStates[0].scope.ignored, true);
  button('Hide ignored feedback').click();
  await tick();
  assert.equal(feedback.querySelector('[data-feedback-thread="linked-thread"]'), null);
  assert.deepEqual(mutations(), []);
});

test('REST-only and missing resolution never inflate ignored unresolved counts; persisted head is not freshness', async () => {
  workspaceRoutes();
  const view = sessionView();
  view.fileStates[0].scope.ignored = true;
  routes['GET /api/v1/session'] = () => json(view);
  const base = { ...view.threads[0], source: 'github', path: view.files[0].path, resolved: false,
    headSha: view.pr.headSha, anchorState: 'outdated', comments: [] };
  let threads = [
    { ...base, id: 'rest-only', resolutionKnown: false, github: { nodeId: 'rest', restOnly: true, isResolved: false, isOutdated: true, comments: [] } },
    { ...base, id: 'missing-resolution', resolutionKnown: false, github: { nodeId: 'missing', isResolved: false, isOutdated: true, comments: [] } },
    { ...base, id: 'confirmed', resolutionKnown: true, github: { nodeId: 'confirmed', isResolved: false, isOutdated: true, comments: [] } },
  ];
  routes['GET /api/v1/github-feedback/threads'] = () => json(page(threads));
  routes['GET /api/v1/github-feedback/records'] = () => json(page([]));
  await start();
  viewButton('feedback').click();
  await tick();
  let feedback = workspaceView('feedback');
  assert.match(feedback.textContent, /1 confirmed unresolved thread on ignored files · 2 with resolution unknown/);
  button('Reveal ignored feedback').click();
  await tick();
  assert.match(feedback.textContent, /Resolution unknown \(REST-only\)/);
  assert.match(feedback.textContent, /Native anchor: outdated/);
  assert.match(feedback.textContent, /freshness unknown/);
  assert.doesNotMatch(feedback.textContent, /native commit matches current head/);
  // Refresh can change the PR head without rewriting the imported thread's
  // persisted head. Native commit evidence, not that field, controls freshness.
  view.pr.headSha = 'new-head';
  threads = threads.map((thread) => ({ ...thread, github: { ...thread.github, comments: [
    { nodeId: 'evidence', databaseId: 7, commitOid: 'old-head', body: '', author: '', createdAt: '' },
  ] } }));
  sources[0].emit('pr.refreshed');
  await settle();
  feedback = workspaceView('feedback');
  assert.match(feedback.textContent, /native commit differs from current head/);
  assert.equal(view.fileStates[0].scope.ignored, true);
  assert.deepEqual(mutations(), []);
});

test('native records restart bounded stale pagination and reject later-page failures', async () => {
  const { fetchFeedbackRecords, ResultsChangedError } = await import('./api.ts');
  let attempts = 0;
  routes['GET /api/v1/github-feedback/records'] = (url) => {
    attempts++;
    if (url.searchParams.has('cursor') && attempts === 2) return json({ error: 'cursor_stale' }, 409);
    return url.searchParams.has('cursor') ? json(page([{ resourceKind: 'github_status', native: { key: 'last' } }]))
      : json({ ...page([{ resourceKind: 'github_review', native: { key: 'first' } }]), hasMore: true, nextCursor: 'next' });
  };
  assert.deepEqual((await fetchFeedbackRecords()).items.map((record) => record.resourceKind), ['github_review', 'github_status']);
  assert.equal(attempts, 4);
  routes['GET /api/v1/github-feedback/records'] = (url) => url.searchParams.has('cursor')
    ? json({ error: 'cursor_stale' }, 409) : json({ ...page([]), hasMore: true, nextCursor: 'next' });
  await assert.rejects(fetchFeedbackRecords(), ResultsChangedError);
  routes['GET /api/v1/github-feedback/records'] = (url) => url.searchParams.has('cursor')
    ? json({ error: 'server_unavailable' }, 503) : json({ ...page([]), hasMore: true, nextCursor: 'next' });
  await assert.rejects(fetchFeedbackRecords(), /503/);
});

test('later-page native error is explicit and does not imply complete-empty', async () => {
  workspaceRoutes();
  routes['GET /api/v1/github-feedback/threads'] = () => json(page([]));
  routes['GET /api/v1/github-feedback/records'] = (url) => url.searchParams.has('cursor')
    ? json({ error: 'backend_failure', message: 'second page failed' }, 500)
    : json({ ...page([{ resourceKind: 'github_review', native: {} }]), hasMore: true, nextCursor: 'next' });
  await start();
  viewButton('feedback').click();
  await tick();
  assert.match(workspaceView('feedback').textContent, /Feedback load failed: second page failed/);
  assert.match(workspaceView('feedback').textContent, /Search results are incomplete/);
  assert.doesNotMatch(workspaceView('feedback').textContent, /No other imported feedback \(complete sources\)/);
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
