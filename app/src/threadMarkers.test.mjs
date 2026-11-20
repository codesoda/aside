import assert from 'node:assert/strict';
import { test } from 'node:test';
import { orderThreads, threadsForLine } from './threadMarkers.ts';

const row = { kind: 'context', oldLine: 10, newLine: 12 };
const thread = (id, side, line, anchorState = 'current') => ({ id, side, line, anchorState });

test('context rows mark threads on either old or new side', () => {
  const threads = [thread('old', 'LEFT', 10), thread('new', 'RIGHT', 12), thread('elsewhere', 'RIGHT', 10)];
  assert.deepEqual(threadsForLine(row, threads).map(({ id }) => id), ['old', 'new']);
});

test('outdated, orphaned, and unknown anchors never mark current code', () => {
  const threads = [thread('outdated', 'RIGHT', 12, 'outdated'), thread('orphan', 'LEFT', 10, 'orphaned'), thread('unknown', 'RIGHT', null)];
  assert.deepEqual(threadsForLine(row, threads), []);
  assert.deepEqual(threadsForLine({ ...row, kind: 'meta' }, [thread('meta', 'RIGHT', 12)]), []);
});

test('deleted lines match the old side without matching unrelated new anchors', () => {
  const threads = [thread('old', 'LEFT', 10), thread('new', 'RIGHT', 12)];
  assert.deepEqual(threadsForLine({ kind: 'deletion', oldLine: 10, newLine: null }, threads).map(({ id }) => id), ['old']);
});

test('right-hand thread cards sort by line and keep unknown locations last', () => {
  const input = [thread('unknown', 'RIGHT', null), thread('later', 'RIGHT', 20), thread('first', 'LEFT', 2)];
  assert.deepEqual(orderThreads(input).map(({ id }) => id), ['first', 'later', 'unknown']);
  assert.equal(input[0].id, 'unknown');
});
