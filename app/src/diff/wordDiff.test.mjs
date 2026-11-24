import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { changedSegments, hunkWordChanges } from './wordDiff.ts';

const line = (id, kind, text) => ({ id, kind, text, oldLine: 1, newLine: 1, patchPosition: 1 });

test('multiple separated changed spans retain exact Unicode text', () => {
  const before = 'const café = old + α; // first';
  const after = 'const café = new + β; // first';
  const [old, next] = changedSegments(before, after);
  assert.equal(old.map((s) => s.text).join(''), before);
  assert.equal(next.map((s) => s.text).join(''), after);
  assert.deepEqual(old.filter((s) => s.changed).map((s) => s.text), ['old', 'α']);
  assert.deepEqual(next.filter((s) => s.changed).map((s) => s.text), ['new', 'β']);
});

test('pairs only bounded adjacent delete/add blocks and leaves unrelated lines alone', () => {
  const hunk = { lines: [line('a', 'deletion', 'old one'), line('b', 'deletion', 'old two'), line('c', 'addition', 'new one'), line('d', 'context', 'same'), line('e', 'addition', 'unpaired')] };
  const changes = hunkWordChanges(hunk);
  assert.equal(changes.has('a'), true);
  assert.equal(changes.has('c'), true);
  assert.equal(changes.has('b'), false);
  assert.equal(changes.has('e'), false);
});

test('oversized line or block skips expensive pairing', () => {
  assert.equal(changedSegments('x'.repeat(2001), 'new'), null);
  const lines = [...Array(13)].map((_, i) => line(`d${i}`, 'deletion', 'old'));
  lines.push(line('a', 'addition', 'new'));
  assert.equal(hunkWordChanges({ lines }).size, 0);
});

test('keeps a changed grapheme intact and bounds all hunks with one budget', () => {
  const before = 'cafe\u0301 👩‍💻';
  const after = 'cafes 👩‍🔬';
  const [old] = changedSegments(before, after);
  assert.deepEqual(old.filter((part) => part.changed).map((part) => part.text), ['cafe\u0301', '👩‍💻']);
  const hunk = { lines: [line('a', 'deletion', before), line('b', 'addition', after)] };
  const budget = { pairs: 1 };
  assert.equal(hunkWordChanges(hunk, budget).size, 2);
  assert.equal(hunkWordChanges(hunk, budget).size, 0);
});

test('plain strings including angle brackets remain untouched', () => {
  const input = '<script>alert("x")</script>';
  const [old] = changedSegments(input, '<script>alert("y")</script>');
  assert.equal(old.map((s) => s.text).join(''), input);
});
