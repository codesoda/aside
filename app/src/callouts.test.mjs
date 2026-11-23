import test from 'node:test';
import assert from 'node:assert/strict';
import { placeCallouts } from './callouts.ts';

const file = { id: 'f', path: 'new.ts', diffDigest: 'd', patchComplete: true, hunks: [{ lines: [
  { oldLine: 4, newLine: 7 }, { oldLine: 5, newLine: 8 }, { oldLine: null, newLine: 9 },
] }] };
const anchor = { kind: 'diff_line', fileId: 'f', currentPath: 'new.ts', diffDigest: 'd', side: 'RIGHT', line: 8, startLine: null };
function item(id, overrides = {}) {
  return {
    label: 'current', latestSnapshotId: 's', latestApplicability: 'current_exact',
    finding: { id, observedSnapshotId: 's', assertionKind: 'measured_fact', validation: { status: 'not_run' }, anchor: { ...anchor }, relatedAnchors: [], ...overrides },
  };
}
const place = (items, files = [file], snapshot = 's', filter = 'all', threads = []) => placeCallouts(items, files, snapshot, filter, threads);

test('places exact old/new sides and ranges at their end row, preserving anchors', () => {
  const left = item('left', { anchor: { ...anchor, side: 'LEFT', line: 5 } });
  const range = item('range', { anchor: { ...anchor, kind: 'diff_range', line: 9, startLine: 7 } });
  const result = place([left, range]);
  assert.deepEqual(result.inline.map(({ side, line, startLine }) => [side, line, startLine]), [['LEFT', 5, 5], ['RIGHT', 9, 7]]);
  assert.equal(result.inline[1].anchor, range.finding.anchor);
});

test('file anchors stay file-level; missing or wrong-side rows stay unplaced', () => {
  const whole = item('whole', { anchor: { ...anchor, kind: 'file', line: null, startLine: null, side: null } });
  const absent = item('absent', { anchor: { ...anchor, side: 'LEFT', line: 9 } });
  const gap = item('gap', { anchor: { ...anchor, kind: 'diff_range', startLine: 6, line: 8 } });
  const result = place([whole, absent, gap]);
  assert.equal(result.file[0].fileId, 'f');
  assert.deepEqual(result.unplaced.map((x) => x.reason), ['missing_row', 'missing_row']);
});

test('snapshot and digest mismatches, related and incomplete patches never inline', () => {
  const related = item('related'); related.latestApplicability = 'current_related';
  const stale = item('stale'); stale.finding.observedSnapshotId = 'old';
  const digest = item('digest'); digest.finding.anchor.diffDigest = 'old';
  const incomplete = item('incomplete');
  assert.deepEqual(place([related, stale, digest]).unplaced.map((x) => x.reason), ['not_exact', 'diff_mismatch']);
  assert.deepEqual(place([stale]).inline.map((x) => x.item.finding.id), ['stale'], 'latest exact applicability supersedes old observation');
  const noDigest = item('no-digest'); noDigest.finding.anchor.diffDigest = null;
  assert.equal(place([noDigest]).inline.length, 1);
  assert.equal(place([incomplete], [{ ...file, patchComplete: false }]).unplaced[0].reason, 'incomplete_patch');
  assert.equal(place([item('x')], [file], 'new').unplaced[0].reason, 'snapshot_mismatch');
});

test('projections and historical items are excluded; related anchors do not relocate', () => {
  const projection = item('projection', { anchor: { ...anchor, githubThreadId: 'gh' } });
  const historical = item('historical'); historical.label = 'historical';
  const unrelated = item('related-anchor', { anchor: { ...anchor, line: 99 }, relatedAnchors: [{ ...anchor }] });
  const result = place([projection, historical, unrelated], [file], 's', 'all', [{ githubThreadId: 'gh' }]);
  assert.deepEqual(result.file, []);
  assert.deepEqual(result.inline, []);
  assert.deepEqual(result.unplaced.map((x) => x.item.finding.id), ['related-anchor']);
  assert.equal(result.unplaced[0].reason, 'missing_row');
});

test('insight class filtering retains server order', () => {
  const suggestion = item('suggestion', { assertionKind: 'informational' });
  const result = place([item('a'), suggestion, item('b')], [file], 's', 'measured');
  assert.deepEqual(result.inline.map((x) => x.item.finding.id), ['a', 'b']);
});
