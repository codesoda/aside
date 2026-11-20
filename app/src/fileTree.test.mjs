import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildFileTree, nextUnreviewedFile } from './fileTree.ts';

test('groups nested files by full directory path without losing root files', () => {
  const tree = buildFileTree([
    { id: 'c', path: 'src/lib/c.rs' },
    { id: 'a', path: 'README.md' },
    { id: 'b', path: 'src/api/b.rs' },
    { id: 'd', path: 'tests/api/d.rs' },
  ]);
  assert.deepEqual(tree.files.map(({ path }) => path), ['README.md']);
  assert.deepEqual(tree.folders.map(({ path }) => path), ['src', 'tests']);
  assert.deepEqual(tree.folders[0].folders.map(({ path }) => path), ['src/api', 'src/lib']);
  assert.deepEqual(tree.folders[1].folders[0].files.map(({ path }) => path), ['tests/api/d.rs']);
});

test('after marking viewed, advances past reviewed and ignored files and wraps to the first remaining file', () => {
  const files = ['a', 'b', 'c', 'd'].map((id) => ({ id }));
  const states = [
    { fileId: 'a', reviewStatus: 'unreviewed', scope: { ignored: false } },
    { fileId: 'b', reviewStatus: 'reviewed-current', scope: { ignored: false } },
    { fileId: 'c', reviewStatus: 'unreviewed', scope: { ignored: true } },
    { fileId: 'd', reviewStatus: 'changed-since-review', scope: { ignored: false } },
  ];
  assert.equal(nextUnreviewedFile(files, states, 'a')?.id, 'd');
  assert.equal(nextUnreviewedFile(files, states, 'd')?.id, 'a');
  assert.equal(nextUnreviewedFile(files, states, 'missing'), undefined);
  states[0].reviewStatus = 'reviewed-current';
  assert.equal(nextUnreviewedFile(files, states, 'd'), undefined);
});

test('repeated folder names remain separate and input order is not mutated', () => {
  const files = [{ path: 'z/shared/b.rs' }, { path: 'a/shared/a.rs' }];
  const tree = buildFileTree(files);
  assert.deepEqual(tree.folders.map(({ path }) => path), ['a', 'z']);
  assert.equal(tree.folders[0].folders[0].path, 'a/shared');
  assert.equal(tree.folders[1].folders[0].path, 'z/shared');
  assert.equal(files[0].path, 'z/shared/b.rs');
});
