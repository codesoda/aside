import assert from 'node:assert/strict';
import { test } from 'node:test';
import { emptyFindingsMessage, fileIndicators, filterQueue, presentAssertion, verdictMessage } from './insights.ts';

test('classes come from AssertionKind; unknown kinds are never shown as measured', () => {
  assert.equal(presentAssertion('measured_fact').cls, 'measured');
  assert.equal(presentAssertion('external_feedback').cls, 'external');
  assert.equal(presentAssertion('attention_hint').cls, 'suggestion');
  assert.equal(presentAssertion('defect_hypothesis', 'unverified').label, 'Unverified hypothesis');
  assert.equal(presentAssertion('defect_hypothesis', 'supported').label, 'Validated hypothesis');
  const unknown = presentAssertion('future_kind');
  assert.equal(unknown.cls, 'suggestion');
  assert.match(unknown.label, /future_kind/);
});

test('only complete coverage may say nothing was found', () => {
  assert.match(emptyFindingsMessage({ overall: 'complete' }), /Every analysis source completed/);
  assert.match(emptyFindingsMessage({ overall: 'pending' }), /still running/);
  assert.match(emptyFindingsMessage({ overall: 'incomplete' }), /not an all-clear/);
  assert.match(emptyFindingsMessage({ overall: 'something_new' }), /not an all-clear/);
  assert.match(verdictMessage({ kind: 'empty_incomplete' }), /not an all-clear/);
  assert.match(verdictMessage({ kind: 'empty_pending' }), /still running/);
});

const item = (fileId, bucket, extra = {}) => ({ fileId, path: fileId, bucket, attention: 'none', reasons: [], scope: { ignored: bucket === 'ignored' }, ...extra });

test('queue filters keep server order and hide ignored files until revealed', () => {
  const items = [
    item('a', 'unreviewed_with_feedback', { attention: 'high', reasons: [{ code: 'unresolved_thread' }] }),
    item('b', 'needs_review'),
    item('c', 'reviewed'),
    item('d', 'ignored', { attention: 'high' }),
  ];
  const ids = (list) => list.map((i) => i.fileId);
  assert.deepEqual(ids(filterQueue(items, 'remaining', false)), ['a', 'b']);
  assert.deepEqual(ids(filterQueue(items, 'remaining', true)), ['a', 'b', 'd']);
  assert.deepEqual(ids(filterQueue(items, 'high', false)), ['a']);
  assert.deepEqual(ids(filterQueue(items, 'high', true)), ['a', 'd']);
  assert.deepEqual(ids(filterQueue(items, 'feedback', false)), ['a']);
  assert.deepEqual(ids(filterQueue(items, 'reviewed', false)), ['c']);
  assert.deepEqual(ids(filterQueue(items, 'ignored', false)), ['d']);
  assert.deepEqual(ids(filterQueue(items, 'all', false)), ['a', 'b', 'c']);
});

test('indicators always carry text, not only a glyph or colour', () => {
  const indicators = fileIndicators(item('a', 'ignored', { attention: 'high', reasons: [{ code: 'unresolved_thread' }, { code: 'unresolved_thread' }] }));
  assert.deepEqual(indicators.map((i) => i.label), ['Ignored by a repository rule', 'High attention', '2 unresolved GitHub threads']);
  for (const i of indicators) assert.ok(i.short.length > 0);
  assert.deepEqual(fileIndicators(undefined), []);
});
