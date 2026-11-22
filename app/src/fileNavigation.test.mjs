import assert from 'node:assert/strict';
import { test } from 'node:test';
import { extensionCounts, extensionLabel, extensionOf, navigationRows, pathMatcher, visibleEntries } from './fileNavigation.ts';

const files = ['src/a.ts', 'tests/nested/B.TS', '.gitignore', 'README', 'src/a.rs'].map((path, index) => ({ id: String(index), path }));
const states = files.map((file, index) => ({ fileId: file.id, reviewStatus: index === 0 ? 'reviewed-current' : 'unreviewed', scope: { ignored: index === 1 } }));
const filters = { query: '', regex: false, extensions: ['no-extension', 'dotfile', '.ts', '.TS', '.rs'], view: 'tree', review: 'all', showIgnored: false };

test('literal search is case-insensitive over full nested paths; regex search and errors', () => {
  assert.equal(pathMatcher('TESTS/NESTED', false).matches('tests/nested/B.TS'), true);
  assert.equal(pathMatcher('^src/.*\\.ts$', true).matches('src/a.ts'), true);
  assert.equal(pathMatcher('\\.rs$', true).matches('src/a.rs'), true);
  assert.equal(pathMatcher('src/.*', true).matches('src/deep/a.rs'), true);
  assert.equal(pathMatcher('^src/.+[.]ts$', true).matches('src/a.ts'), true);
  assert.match(pathMatcher('\\d+', true).error, /escaped punctuation/);
  assert.equal(pathMatcher('[', true).error, 'Invalid regex pattern.');
  assert.match(pathMatcher('(a+)+', true).error, /no groups/);
  assert.match(pathMatcher('a*a*a*', true).error, /at most one repetition/);
  assert.deepEqual(visibleEntries(files, states, null, { ...filters, query: 'NESTED', showIgnored: true }).entries.map(({ file }) => file.path), ['tests/nested/B.TS']);
});

test('extension categories match backend basename and case-sensitive preference keys', () => {
  const examples = [
    ['README', 'no-extension'], ['.gitignore', 'dotfile'], ['folder/.config.json', 'dotfile'],
    ['a.ts', '.ts'], ['folder/file.', 'no-extension'], ['folder/name..ts', '.ts'],
    ['folder/FILE.TS', '.TS'], ['folder/.hidden.ts', 'dotfile'],
  ];
  for (const [path, category] of examples) assert.equal(extensionOf(path), category, path);
  assert.equal(extensionLabel('no-extension'), 'No extension');
  assert.equal(extensionLabel('dotfile'), 'Dotfiles');
  assert.equal(extensionLabel('.ts'), '.ts');
  const varied = examples.map(([path], index) => ({ id: String(index), path }));
  assert.deepEqual(Object.fromEntries(extensionCounts(varied).map(({ extension, count }) => [extension, count])),
    { 'no-extension': 2, dotfile: 3, '.ts': 2, '.TS': 1 });
  assert.deepEqual(visibleEntries(varied, [], null, { ...filters, extensions: ['dotfile'] }).entries.map(({ file }) => file.path),
    ['.gitignore', 'folder/.config.json', 'folder/.hidden.ts']);
  assert.deepEqual(extensionCounts(files), [
    { extension: '.rs', count: 1 }, { extension: '.ts', count: 1 }, { extension: '.TS', count: 1 },
    { extension: 'dotfile', count: 1 }, { extension: 'no-extension', count: 1 },
  ]);
  assert.equal(visibleEntries(files, states, null, filters).entries.length, 4);
  assert.equal(visibleEntries(files, states, null, { ...filters, extensions: [] }).entries.length, 0);
  assert.deepEqual(visibleEntries(files, states, null, { ...filters, extensions: ['.ts', '.TS'], showIgnored: true }).entries.map(({ file }) => file.path), ['src/a.ts', 'tests/nested/B.TS']);
  assert.equal(visibleEntries(files, states, null, { ...filters, review: 'reviewed-current' }).entries.length, 1);
  assert.equal(states[1].scope.ignored, true);
});

test('tree creates nested folders and priority follows queue order', () => {
  const entries = visibleEntries(files, states, null, { ...filters, showIgnored: true }).entries;
  const rows = navigationRows(entries, 'tree', new Set());
  assert.deepEqual(rows.filter((row) => row.kind === 'directory').map((row) => row.path), ['src', 'tests', 'tests/nested']);
  assert.equal(navigationRows(entries, 'tree', new Set(['tests'])).some((row) => row.kind === 'file' && row.entry.file.path.includes('nested')), false);
  const queue = { queue: { items: [{ fileId: '1' }, { fileId: '0' }] } };
  assert.deepEqual(visibleEntries(files, states, queue, { ...filters, showIgnored: true, view: 'priority' }).entries.slice(0, 2).map(({ file }) => file.id), ['1', '0']);
});
