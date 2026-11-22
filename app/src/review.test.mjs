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
  document.title = 'Aside'; // Match the static shell, including its existing title element.
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

test('page title resets while loading and identifies the loaded review', async () => {
  const pending = deferred();
  workspaceRoutes();
  routes['GET /api/v1/session'] = () => pending.promise;
  document.title = '#99: Previous review · old/repo · Aside';
  await start();
  assert.equal(document.title, 'Loading review… · Aside');

  pending.resolve(json(sessionView()));
  await settle();
  assert.equal(document.title, '#7: Tidy auth · o/n · Aside');
  assert.equal(document.querySelectorAll('title').length, 1);
});

test('page title follows review changes and renamed PRs from session events', async () => {
  let view = sessionView();
  workspaceRoutes(() => view);
  await start();
  assert.equal(document.title, '#7: Tidy auth · o/n · Aside');

  view = { ...view, sessionId: 's2', pr: { ...view.pr, owner: 'acme', repo: 'widgets', number: 12, title: 'Improve search' } };
  sources[0].emit('resync');
  await settle();
  assert.equal(document.title, '#12: Improve search · acme/widgets · Aside');

  view.pr.title = 'Improve search and filtering';
  sources[0].emit('pr.refreshed');
  await settle();
  assert.equal(document.title, '#12: Improve search and filtering · acme/widgets · Aside');
  assert.deepEqual(mutations(), []);
});

test('late session responses cannot restore a previous review title', async () => {
  const stale = deferred();
  workspaceRoutes();
  routes['GET /api/v1/session'] = () => stale.promise;
  await start();

  const latest = sessionView();
  latest.pr = { ...latest.pr, number: 8, title: 'New review' };
  routes['GET /api/v1/session'] = () => json(latest);
  sources[0].emit('pr.refreshed');
  await settle();
  assert.equal(document.title, '#8: New review · o/n · Aside');

  stale.resolve(json(sessionView()));
  await settle();
  assert.equal(document.title, '#8: New review · o/n · Aside');
});

test('initial and refresh failures clear the title until the session recovers', async () => {
  workspaceRoutes();
  routes['GET /api/v1/session'] = () => json({}, 503);
  await start();
  assert.equal(document.title, 'Review unavailable · Aside');

  workspaceRoutes();
  sources[0].emit('resync');
  await settle();
  assert.equal(document.title, '#7: Tidy auth · o/n · Aside');

  routes['GET /api/v1/session'] = () => json({}, 503);
  sources[0].emit('pr.refreshed');
  await settle();
  assert.equal(document.title, 'Review unavailable · Aside');
  assert.match(text(), /Refresh failed/);

  workspaceRoutes();
  sources[0].emit('resync');
  await settle();
  assert.equal(document.title, '#7: Tidy auth · o/n · Aside');
});

test('page title handles incomplete metadata and treats long Unicode titles as text', async () => {
  const view = sessionView();
  view.pr.title = '';
  delete view.pr.number;
  workspaceRoutes(() => view);
  await start();
  assert.equal(document.title, 'Review · o/n · Aside');

  view.pr.title = '修正 👩🏽‍💻 café & "quotes" </title><script>window.injected = true</script> '.repeat(20).trim();
  sources[0].emit('pr.refreshed');
  await settle();
  assert.equal(document.title, `${view.pr.title} · o/n · Aside`);
  assert.equal(document.querySelector('script'), null);
  assert.equal(window.injected, undefined);
});

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
  assert.match(document.querySelector('#file-review').textContent, /Loading insights/);
  assert.equal(document.querySelector('.callout-marker'), null);
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
  assert.ok(byLabel('Open src/auth.rs'), 'compact file navigation remains available after analysis');
  assert.ok(document.querySelector('.diff-row .marker-group'), 'a finding becomes a selectable dot');
  assert.equal(document.querySelector('[aria-label="Selected conversation"]'), null, 'no default conversation');
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
  assert.ok(diff.querySelector('.workspace-diff-header .marker-group'), 'file insight appears beside file heading');
  const row = [...diff.querySelectorAll('.diff-row')].find((el) => el.textContent.includes('verify_token'));
  assert.equal(diff.querySelector('.diff-row .marker-group').tagName, 'BUTTON');
  assert.ok(row.querySelector('.marker-group'), 'unrelated existing thread keeps its own end-row marker');
  assert.equal(diff.querySelectorAll('.inline-callouts').length, 0, 'no always-visible cards');
  assert.equal(byLabel('Selected conversation'), null);
  assert.deepEqual(mutations(), [], 'loading and rendering findings never writes');
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
  assert.deepEqual([...document.querySelectorAll('.diff-row .marker-group')].filter((marker) => /line 1/.test(marker.getAttribute('aria-label'))).map((marker) => marker.getAttribute('aria-label').match(/(old|new) line 1/)[1]), ['old', 'new']);
  assert.doesNotMatch(row.querySelector('.marker-group')?.getAttribute('aria-label') ?? '', /range/, 'range adds no end-row marker');
  assert.equal(row.querySelector('span.w-9').className.includes('w-9'), true);
  assert.equal(document.querySelectorAll('.inline-callouts').length, 0, 'ranges do not insert cards');
  assert.deepEqual(mutations(), []);
});

test('a file with only suggestions does not claim no insights or make writes', async () => {
  workspaceRoutes();
  routes['GET /api/v1/analysis/findings'] = () => json(page([findings[1]]));
  await start();
  assert.match(document.querySelector('#file-review').textContent, /1 insights/);
  assert.equal(document.querySelectorAll('.diff-row .marker-group').length, 1);
  assert.doesNotMatch(document.querySelector('#file-review').textContent, /No findings for this file/);
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
  assert.equal(document.querySelectorAll('.inline-callouts').length, 0);
  assert.ok(document.querySelector('[aria-label="Outdated and unplaced conversations"]'));
  assert.deepEqual(mutations(), []);
});

test('prep explains and deep-links an attention finding without writing', async () => {
  const queue = queueView();
  queue.queue.items[0].findings = [{ id: 'fnd-measured', current: true, attention: 'high', disposition: 'active' }];
  routes['GET /api/v1/session'] = () => json(sessionView());
  routes['GET /api/v1/analysis/queue'] = () => json(queue);
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
  await start();
  assert.match(workspaceView('prep').textContent, /High attention from a deterministic rule/);
  button('Review finding Changes 2 lines in an auth path in src/auth.rs').click();
  await settle();
  assert.equal(workspaceView('files').hidden, false);
  assert.equal(document.querySelectorAll('.inline-callouts').length, 0);
  assert.deepEqual(mutations().map(({ path, body }) => [path, JSON.parse(body)]), [[
    '/api/v1/threads', { subject: 'line', fileId: 'f-auth', diffDigest: 'digest-f-auth', findingId: 'fnd-measured', side: 'RIGHT', line: 2 },
  ]], 'opening a finding requests its persistent empty conversation, not a review comment');
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
  assert.equal(document.querySelector('.inline-callouts'), null);
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
  // Changing navigation filters does not mutate review state.
  button('File filters').click();
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
  const empty = document.querySelector('#file-review').textContent;
  assert.match(empty, /0 insights/);
  assert.match(status.textContent, /partial/);
  assert.doesNotMatch(text(), /Every analysis source completed|Nothing remains and every source completed/);
  assert.ok(byLabel('Open src/auth.rs'), 'navigation remains available despite incomplete coverage');
});

test('measured facts, suggestions and external feedback are distinct in text and accessible name', async () => {
  routes['GET /api/v1/session'] = () => json(sessionView({ overall: 'complete', sources: [source('aside.diff-facts', 'has_results')] }));
  routes['GET /api/v1/analysis/queue'] = () => json(queueView());
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
  routes['GET /api/v1/analysis/findings/fnd-measured/evidence'] = () => json(page(evidence));
  await start();

  assert.equal(document.querySelectorAll('.inline-callouts').length, 0);
  assert.equal(document.querySelectorAll('.diff-row .marker-group').length, 1, 'native projection shares the line marker with its thread');
  assert.equal(byLabel('Selected conversation'), null, 'no conversation shown by default');
  assert.doesNotMatch(text(), /0\.9[37]/);
  button('Open GitHub thread by bob on src/auth.rs:2').click();
  await settle();
  assert.match(byLabel('Selected conversation').textContent, /GitHub feedback/);
  assert.match(byLabel('Selected conversation').textContent, /bob|carol/);
  assert.deepEqual(mutations(), []);

  // Queue reasons render the #30 reasons with what they cite.
  const why = byLabel('Reasons for src/auth.rs').textContent.replace(/\s+/g, ' ');
  assert.match(why, /Unresolved GitHub thread — thread by bob on line 2/);
  assert.match(why, /High attention from a deterministic rule — finding “Changes 2 lines in an auth path” · provider aside\.diff-facts · rule auth_path/);
});

test('sidebar actions persist explicit review/repository lifetimes; filters and selection do not write', async () => {
  const view = sessionView();
  workspaceRoutes(() => view);
  routes['POST /api/v1/repositories/current/exclusions'] = (_url, init) => {
    const target = JSON.parse(init.body);
    if (target.expression === 'gen/out.rs' && target.action === 'include') {
      view.fileStates[1].scope.ignored = false;
      view.fileStates[1].status = 'unreviewed';
    }
    return json({});
  };
  await start();
  button('File filters').click();
  await tick();
  const reveal = [...document.querySelectorAll('label')].find((label) => label.textContent.includes('Show ignored'))?.querySelector('input');
  reveal.click();
  await tick();
  byLabel('Expand folder gen')?.click();
  await tick();
  [...document.querySelectorAll('button[aria-label]')].find((b) => b.getAttribute('aria-label').startsWith('Open gen/out.rs')).click();
  await tick();
  assert.deepEqual(mutations(), []);
  byLabel('Actions for file gen/out.rs').click();
  await tick();
  document.querySelector('[aria-label="Scope actions for file gen/out.rs"] button').click();
  await settle();
  assert.deepEqual(JSON.parse(mutations()[0].body), { action: 'include', kind: 'file', expression: 'gen/out.rs', lifetime: 'review' });
  assert.equal(view.fileStates[1].scope.ignored, false);
  byLabel('Actions for file gen/out.rs').click();
  await tick();
  [...document.querySelectorAll('[aria-label="Scope actions for file gen/out.rs"] button')].find((b) => b.textContent === 'Exclude always').click();
  await settle();
  assert.deepEqual(JSON.parse(mutations()[1].body), { action: 'exclude', kind: 'file', expression: 'gen/out.rs', lifetime: 'repository' });
  assert.equal(calls.filter((c) => c.path.startsWith('/api/v1/analysis/runs')).length, 0);
});

test('file header scope menu permits individual include and explicit lifetime', async () => {
  const view = sessionView();
  workspaceRoutes(() => view);
  routes['POST /api/v1/repositories/current/exclusions'] = () => json({});
  await start();
  byLabel('Scope actions for src/auth.rs').click();
  await tick();
  const menu = byLabel('Scope actions for file src/auth.rs');
  assert.ok(menu);
  assert.deepEqual([...menu.querySelectorAll('button')].map((b) => b.textContent.trim()), ['Include this review', 'Include always', 'Exclude this review', 'Exclude always']);
  menu.querySelector('button').click();
  await settle();
  assert.deepEqual(JSON.parse(mutations()[0].body), { action: 'include', kind: 'file', expression: 'src/auth.rs', lifetime: 'review' });
});

test('every new insight control is a native, named, keyboard-focusable element with visible focus', async () => {
  routes['GET /api/v1/session'] = () => json(sessionView({ overall: 'complete', sources: [source('aside.diff-facts', 'has_results')] }));
  routes['GET /api/v1/analysis/queue'] = () => json(queueView());
  routes['GET /api/v1/analysis/findings'] = () => json(page(findings));
  await start();
  const regions = [
    document.querySelector('#file-review'),
    document.querySelector('[aria-label="Analysis status"]'),
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
      if (el.matches('.callout-marker, .thread-marker')) assert.match(el.className, /marker/, `marker has focus style: ${el.outerHTML}`);
      el.focus();
      assert.equal(document.activeElement, el, `not focusable: ${el.outerHTML}`);
    }
  }
  assert.ok(checked >= 5, `checked ${checked} controls`);
  assert.equal(document.querySelectorAll('.inline-callouts').length, 0);
  assert.ok(document.querySelectorAll('.diff-row .marker-group').length >= 1);
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
  assert.equal(document.title, '#7: Tidy auth · o/n · Aside');
  assert.equal(diff.querySelector('.diff-row .thread-marker').getAttribute('aria-pressed'), 'true');
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
  button('File filters').click();
  await tick();
  const reveal = [...document.querySelectorAll('label')].find((label) => label.textContent.includes('Show ignored'))?.querySelector('input');
  reveal.click();
  await tick();
  byLabel('Actions for file gen/out.rs').click();
  await tick();
  document.querySelector('[aria-label="Scope actions for file gen/out.rs"] button').click();
  await settle();
  assert.equal(workspaceView('prep').hidden, false);
  assert.deepEqual([...workspaceView('prep').querySelectorAll('dd')].map((el) => el.textContent), ['2', '0', '0']);
  assert.deepEqual(mutations().map((write) => JSON.parse(write.body)), [{ action: 'include', kind: 'file', expression: 'gen/out.rs', lifetime: 'review' }]);
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
  diff.querySelector('.diff-row .thread-marker').click();
  await tick();
  fill('reply-desktop-t1', 'My local reply');
  await tick();
  button('Send private reply').click();
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
    document.querySelector('.diff-row .thread-marker').click();
    await tick();
    const input = document.getElementById('reply-desktop-t1');
    assert.ok(input, 'reply editor is present only after selecting a thread');
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
    assert.equal(document.getElementById('reply-desktop-t1').value, '');
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

test('selected conversation shows grouped unread marker, private reply and local resolution without outgoing draft', async () => {
  const view = sessionView();
  const thread = view.threads[0];
  Object.assign(thread, { source: 'local', subject: 'line', unreadCount: 2, lastParticipant: 'agent',
    agent: { state: 'idle' }, reviewComment: null, findingId: null });
  const second = structuredClone(thread);
  second.id = 't2'; second.comments = []; second.unreadCount = 0;
  view.threads.push(second);
  workspaceRoutes(() => view);
  routes['PATCH /api/v1/threads/t1'] = (_url, init) => {
    const update = JSON.parse(init.body);
    if ('resolved' in update) thread.resolved = update.resolved;
    if (update.readThrough) thread.unreadCount = 0;
    return json({ thread });
  };
  routes['POST /api/v1/threads/t1/replies'] = (_url, init) => {
    thread.comments.push({ id: 'new-reply', author: 'human', authorKind: 'human', body: JSON.parse(init.body).body, publishState: 'private' });
    return json({});
  };
  await start();
  const marker = document.querySelector('.diff-row .thread-marker');
  assert.match(marker.className, /has-unread/);
  assert.match(marker.style.cssText, /participant/);
  assert.match(marker.parentElement.textContent, /[2-9]/, 'group count is visible');
  assert.match(marker.getAttribute('aria-label'), /unread; last agent/);
  assert.equal(byLabel('Selected conversation'), null);
  marker.click(); await settle();
  const panel = byLabel('Selected conversation');
  assert.ok(panel);
  assert.equal(document.querySelectorAll('.desktop-conversation .review-thread').length, 1);
  assert.deepEqual(JSON.parse(mutations()[0].body), { readThrough: thread.comments.at(-1).id });
  assert.equal(thread.unreadCount, 0);
  const input = document.getElementById('reply-desktop-t1');
  input.value = 'Private question'; input.dispatchEvent(new window.Event('input', { bubbles: true })); await tick();
  button('Send private reply').click(); await settle();
  assert.equal(thread.comments.at(-1).body, 'Private question');
  assert.equal(thread.reviewComment, null, 'discussion never creates an outgoing review comment');
  button('Mark handled locally').click(); await settle();
  assert.equal(thread.resolved, true);
  button('Reopen locally').click(); await settle();
  assert.equal(thread.resolved, false);
  assert.equal(mutations().filter((call) => call.path.includes('review-comment')).length, 0);
  button('Next').click(); await tick();
  assert.equal(document.querySelector('.desktop-conversation article').id, 'thread-t2');
});

test('both panel instances share per-thread unsaved text through resize, refresh and thread selection', async () => {
  const view = sessionView();
  const first = view.threads[0];
  Object.assign(first, { source: 'local', subject: 'line', agent: { state: 'idle' }, reviewComment: null });
  const second = structuredClone(first);
  second.id = 't2'; second.comments = [];
  view.threads.push(second);
  workspaceRoutes(() => view);
  const media = window.matchMedia;
  let mobile = false;
  window.matchMedia = () => ({ matches: mobile });
  try {
    await start();
    document.querySelector('.diff-row .thread-marker').click(); await settle();
    const desktop = document.getElementById('reply-desktop-t1');
    const inline = document.getElementById('reply-mobile-t1');
    assert.ok(desktop && inline);
    assert.equal(document.querySelectorAll('#reply-desktop-t1').length, 1);
    assert.equal(document.querySelectorAll('#reply-mobile-t1').length, 1);
    const label = document.querySelector('.desktop-conversation label');
    assert.equal(label.control, desktop, 'desktop label targets its visible editor');
    desktop.focus();
    assert.equal(document.activeElement, desktop);
    const fill = (element, value) => {
      element.value = value;
      element.dispatchEvent(new window.Event('input', { bubbles: true }));
    };
    fill(desktop, 'Unsent private reply');
    fill(document.querySelector('.desktop-conversation [aria-label="Review comment draft"]'), 'Unsent outgoing edit');
    await tick();
    assert.equal(inline.value, 'Unsent private reply');
    assert.equal(document.querySelector('.mobile-conversation [aria-label="Review comment draft"]').value, 'Unsent outgoing edit');
    mobile = true; window.dispatchEvent(new window.Event('resize'));
    sources[0].emit('resync'); await settle();
    assert.equal(inline.value, 'Unsent private reply');
    assert.equal(document.querySelector('.mobile-conversation [aria-label="Review comment draft"]').value, 'Unsent outgoing edit');
    button('Next').click(); await settle();
    assert.equal(document.getElementById('reply-mobile-t2').value, '');
    fill(document.getElementById('reply-mobile-t2'), 'Other thread'); await tick();
    button('Previous').click(); await settle();
    assert.equal(document.getElementById('reply-mobile-t1').value, 'Unsent private reply');
    assert.equal(document.querySelector('.desktop-conversation [aria-label="Review comment draft"]').value, 'Unsent outgoing edit');
    button('Next').click(); await settle();
    assert.equal(document.getElementById('reply-desktop-t2').value, 'Other thread');
    assert.deepEqual(mutations(), [], 'drafting and selecting threads never writes');
  } finally { window.matchMedia = media; }
});

test('failed outgoing save keeps the shared edit and successful clear resets both panels', async () => {
  const view = sessionView();
  const thread = view.threads[0];
  Object.assign(thread, { source: 'local', subject: 'line', agent: { state: 'idle' }, reviewComment: null });
  workspaceRoutes(() => view);
  routes['PUT /api/v1/threads/t1/review-comment'] = () => json({ message: 'save failed' }, 500);
  routes['DELETE /api/v1/threads/t1/review-comment'] = () => { thread.reviewComment = null; return json({}); };
  await start(); document.querySelector('.diff-row .thread-marker').click(); await settle();
  const editor = document.querySelector('.desktop-conversation [aria-label="Review comment draft"]');
  editor.value = 'Keep this text'; editor.dispatchEvent(new window.Event('input', { bubbles: true })); await tick();
  [...document.querySelectorAll('.desktop-conversation button')].find((item) => item.textContent === 'Save review comment').click(); await settle();
  assert.equal(editor.value, 'Keep this text');
  assert.equal(document.querySelector('.mobile-conversation [aria-label="Review comment draft"]').value, 'Keep this text');
  routes['PUT /api/v1/threads/t1/review-comment'] = (_url, init) => { thread.reviewComment = { id: 'saved', body: JSON.parse(init.body).body }; return json({}); };
  [...document.querySelectorAll('.desktop-conversation button')].find((item) => item.textContent === 'Save review comment').click(); await settle();
  [...document.querySelectorAll('.desktop-conversation button')].find((item) => item.getAttribute('aria-label') === 'Clear outgoing review comment').click(); await settle();
  assert.equal(document.querySelector('.desktop-conversation [aria-label="Review comment draft"]').value, '');
  assert.equal(document.querySelector('.mobile-conversation [aria-label="Review comment draft"]').value, '');
});

test('outgoing editor saves only explicitly, clears and copies agent verbatim with overwrite confirmation', async () => {
  const view = sessionView();
  const thread = view.threads[0];
  Object.assign(thread, { source: 'local', subject: 'line', agent: { state: 'idle' }, reviewComment: null });
  thread.comments.push({ id: 'agent-reply', author: 'agent', authorKind: 'agent', body: 'Exact agent text.', publishState: 'private' });
  workspaceRoutes(() => view);
  routes['PUT /api/v1/threads/t1/review-comment'] = (_url, init) => {
    thread.reviewComment = { id: 'draft', body: JSON.parse(init.body).body }; return json({});
  };
  routes['DELETE /api/v1/threads/t1/review-comment'] = () => { thread.reviewComment = null; return json({}); };
  await start(); document.querySelector('.diff-row .thread-marker').click(); await settle();
  button('+ Add to review').click(); await settle();
  assert.deepEqual(JSON.parse(mutations().at(-1).body), { body: 'Exact agent text.', sourceCommentId: 'agent-reply' });
  button('Clear outgoing review comment').click(); await settle();
  assert.equal(thread.reviewComment, null);
  assert.equal(document.querySelector('.desktop-conversation [aria-label="Review comment draft"]').value, '');
  assert.equal(document.querySelector('.desktop-conversation button.primary-button')?.disabled ?? button('Save review comment').disabled, true);
  assert.equal(mutations().filter((call) => call.path.endsWith('/replies')).length, 0);
});

test('repository type preference reloads and failure does not falsely save', async () => {
  workspaceRoutes();
  let rules = [];
  routes['GET /api/v1/repositories/current/extension-preferences'] = () => json({ rules });
  routes['PUT /api/v1/repositories/current/extension-preferences/.rs'] = (_url, init) => {
    const mode = JSON.parse(init.body).mode;
    rules = mode === 'default' ? [] : [{ category: '.rs', action: mode, enabled: true }];
    return json({ rule: rules[0] ?? null });
  };
  await start();
  const search = byLabel('Search file paths');
  search.value = 'auth'; search.dispatchEvent(new window.Event('input', { bubbles: true }));
  button('File filters').click(); await tick();
  document.querySelector('nav[aria-label="Changed files"] label.regex input').click(); await tick();
  let select = byLabel('Repository preference for .rs');
  select.value = 'exclude'; select.dispatchEvent(new window.Event('change', { bubbles: true })); await settle();
  assert.equal(byLabel('Repository preference for .rs').value, 'exclude');
  assert.equal(JSON.parse(mutations().at(-1).body).mode, 'exclude');
  sources[0].emit('resync'); await settle();
  assert.equal(byLabel('Repository preference for .rs').value, 'exclude');
  routes['PUT /api/v1/repositories/current/extension-preferences/.rs'] = () => json({ message: 'cannot save' }, 500);
  select = byLabel('Repository preference for .rs');
  select.value = 'include'; select.dispatchEvent(new window.Event('change', { bubbles: true })); await settle();
  assert.equal(byLabel('Repository preference for .rs').value, 'exclude', 'failed choice is not persisted');
  assert.equal(byLabel('Search file paths').value, 'auth', 'preference reload preserves search');
  assert.equal(document.querySelector('nav[aria-label="Changed files"] label.regex input').checked, true, 'preference reload preserves regex');
  assert.match(document.querySelector('.workspace-sidebar [role="alert"]').textContent, /cannot save/);
});

test('selected range aligns desktop panel to starting row and renders mobile panel immediately beneath it', async () => {
  const view = sessionView();
  Object.assign(view.threads[0], { source: 'local', subject: 'line', side: 'RIGHT', startLine: 1, line: 2,
    agent: { state: 'idle' }, unreadCount: 0 });
  workspaceRoutes(() => view);
  const original = HTMLElement.prototype.getBoundingClientRect;
  const media = window.matchMedia;
  window.matchMedia = () => ({ matches: false });
  HTMLElement.prototype.getBoundingClientRect = function () {
    const top = this.id === 'file-review' ? 10 : this.dataset.threadAnchor?.split(' ').includes('t1') ? 150
      : this.matches?.('.desktop-conversation .review-thread') ? 40 + parseFloat(document.querySelector('.desktop-conversation')?.style.marginTop || '0') : 0;
    return { top, left: 0, right: 0, bottom: 0, width: 0, height: 0 };
  };
  try {
    await start();
    viewButton('files').click(); await tick();
    const row = document.querySelector('[data-thread-anchor="t1"]');
    assert.equal(row.querySelector('[data-code-line]').dataset.codeLine, '1');
    assert.equal(row.getBoundingClientRect().top, 150);
    assert.equal(document.getElementById('file-review').getBoundingClientRect().top, 10);
    row.querySelector('.thread-marker').click(); await settle();
    assert.equal(document.querySelector('.desktop-conversation').style.marginTop, '110px');
    const inline = row.nextElementSibling;
    assert.match(inline.className, /mobile-conversation/);
    assert.equal(inline.querySelector('article').id, 'thread-mobile-t1');
    assert.equal(document.querySelector('.desktop-conversation article').id, 'thread-t1');
    window.dispatchEvent(new window.Event('resize')); await settle();
    assert.equal(document.querySelector('.desktop-conversation').style.marginTop, '110px');
  } finally { HTMLElement.prototype.getBoundingClientRect = original; window.matchMedia = media; }
});

test('repository owner matching PR author is not proof of self review', async () => {
  const view = sessionView();
  view.demo = false; view.pr.owner = view.pr.author;
  workspaceRoutes(() => view);
  await start();
  button('Submit review — Draft (0)').click(); await settle();
  assert.equal(byLabel('Review verdict').querySelector('[value="APPROVE"]').disabled, false);
  assert.deepEqual(mutations(), []);
});

test('opening Submit Review never posts a review', async () => {
  workspaceRoutes(() => ({ ...sessionView(), demo: false }));
  await start();
  button('Submit review — Draft (0)').click();
  await settle();
  assert.ok(byLabel('Submit review'));
  assert.equal(mutations().filter((call) => call.path.startsWith('/api/v1/review/publish')).length, 0);
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
