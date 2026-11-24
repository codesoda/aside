import assert from 'node:assert/strict';
import { test } from 'node:test';
import { acceptsSession, createReviewSession, isCurrentFile, selectedFile } from './reviewSession.ts';
import { watchSession } from './api.ts';

const view = (sequence, sessionId = 'one', files = [{ id: 'a', diffDigest: 'old' }]) => ({ sequence, sessionId, files });
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};

test('resync ignores reordered views and rejects a different session identity', () => {
  const seen = [], errors = [];
  let update, fail, stopped = false;
  const live = createReviewSession((item) => seen.push(item.sequence), (message) => errors.push(message), {
    watch(onUpdate, onError) { update = onUpdate; fail = onError; return () => { stopped = true; }; },
    fetch: async () => view(0),
  });
  update(view(4)); update(view(2)); update(view(5)); update(view(6, 'other'));
  assert.deepEqual(seen, [4, 5]);
  assert.match(errors.at(-1), /session changed/);
  fail('disconnected'); update(view(7));
  assert.equal(live.blocked, true);
  assert.match(errors.at(-1), /session changed/);
  live.stop(); update(view(8)); fail('late error');
  assert.equal(stopped, true);
  assert.deepEqual(seen, [4, 5]);
  assert.match(errors.at(-1), /session changed/);
});

test('reconnect and action refresh cannot overwrite a newer snapshot', async () => {
  const seen = [], first = deferred(), second = deferred();
  let update;
  let calls = 0;
  const live = createReviewSession((item) => seen.push(item.sequence), () => {}, {
    watch(onUpdate) { update = onUpdate; return () => {}; },
    fetch() { return ++calls === 1 ? first.promise : second.promise; },
  });
  update(view(8));
  const early = live.refresh(); const late = live.refresh();
  second.resolve(view(10)); await late;
  first.resolve(view(9)); await early;
  update(view(9));
  assert.deepEqual(seen, [8, 10]);
  assert.equal(live.current.sequence, 10);
  live.stop();
});

test('SSE reconnect reloads the authoritative view; cleanup ignores a late response', async () => {
  const previousFetch = globalThis.fetch;
  const previousEventSource = globalThis.EventSource;
  const pending = [];
  let source;
  globalThis.EventSource = class {
    listeners = new Map();
    closed = false;
    constructor() { source = this; }
    addEventListener(name, listener) { this.listeners.set(name, listener); }
    close() { this.closed = true; }
  };
  globalThis.fetch = async () => {
    const request = deferred();
    pending.push(request);
    return { ok: true, json: () => request.promise };
  };
  try {
    const seen = [];
    const stop = watchSession((snapshot) => seen.push(snapshot.sequence), () => {});
    assert.equal(pending.length, 1);
    source.onopen(); // reconnect triggers a second authoritative fetch
    assert.equal(pending.length, 2);
    pending[1].resolve(view(5));
    await new Promise((resolve) => setImmediate(resolve));
    pending[0].resolve(view(4));
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(seen, [5]);
    source.listeners.get('file.reviewed')();
    assert.equal(pending.length, 3);
    stop();
    pending[2].resolve(view(6));
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(seen, [5]);
    assert.equal(source.closed, true);
  } finally {
    globalThis.fetch = previousFetch;
    globalThis.EventSource = previousEventSource;
  }
});

test('selection falls back safely and digest guard rejects an outdated patch', () => {
  const snapshot = view(1, 'one', [{ id: 'a', diffDigest: 'new' }, { id: 'b', diffDigest: 'b' }]);
  assert.equal(selectedFile(snapshot, 'missing').id, 'a');
  assert.equal(selectedFile(snapshot, 'b').id, 'b');
  assert.equal(isCurrentFile(snapshot, 'a', 'old'), false);
  assert.equal(isCurrentFile(snapshot, 'a', 'new'), true);
  assert.equal(acceptsSession(view(3), view(2)), false);
  assert.equal(acceptsSession(view(3), view(4, 'other')), false);
});
