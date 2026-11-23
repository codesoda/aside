// Read-only placement of insight cards. A line is never inferred from an old
// coordinate, a related anchor, or a similar-looking path.
import type { ChangedFile } from './generated/ChangedFile';
import type { CodeAnchor } from './generated/CodeAnchor';
import type { FindingItem } from './generated/FindingItem';
import { isThreadProjection, presentAssertion, type FindingFilter } from './insights';
import type { ReviewThread } from './generated/ReviewThread';

export type Callout = {
  item: FindingItem;
  /** The stored primary anchor, unchanged; related anchors are not placements. */
  anchor: CodeAnchor;
};
export type FileCallout = Callout & { fileId: string };
export type InlineCallout = FileCallout & { side: 'LEFT' | 'RIGHT'; line: number; startLine: number };
export type UnplacedReason = 'snapshot_mismatch' | 'not_exact' | 'no_file' | 'anchor_mismatch' | 'diff_mismatch' | 'incomplete_patch' | 'missing_row' | 'unsupported_anchor';
export type UnplacedCallout = Callout & { reason: UnplacedReason };
export type CalloutPlacements = {
  file: FileCallout[];
  inline: InlineCallout[];
  unplaced: UnplacedCallout[];
};

/** Preserve server order within each placement group and insight-class filter. */
export function placeCallouts(
  items: FindingItem[], files: ChangedFile[], snapshotId: string,
  filter: FindingFilter = 'all', threads: ReviewThread[] = [],
): CalloutPlacements {
  const result: CalloutPlacements = { file: [], inline: [], unplaced: [] };
  for (const item of items) {
    const finding = item.finding;
    if (item.label !== 'current' || isThreadProjection(finding, threads)) continue;
    if (filter !== 'all' && presentAssertion(finding.assertionKind, finding.validation.status).cls !== filter) continue;
    const anchor = finding.anchor;
    const callout: Callout = { item, anchor };
    const unplaced = (reason: UnplacedReason) => result.unplaced.push({ ...callout, reason });
    if (item.latestSnapshotId !== snapshotId) {
      unplaced('snapshot_mismatch');
      continue;
    }
    if (item.latestApplicability !== 'current_exact') {
      unplaced('not_exact');
      continue;
    }
    if (anchor.kind === 'pr' || !anchor.currentPath || !anchor.fileId) {
      unplaced('no_file');
      continue;
    }
    const file = files.find((f) => f.id === anchor.fileId && f.path === anchor.currentPath);
    if (!file) {
      unplaced('anchor_mismatch');
      continue;
    }
    if (anchor.diffDigest !== null && anchor.diffDigest !== file.diffDigest) {
      unplaced('diff_mismatch');
      continue;
    }
    if (anchor.line === null && anchor.startLine === null && (anchor.kind === 'file' || anchor.kind === 'entity')) {
      result.file.push({ ...callout, fileId: file.id });
      continue;
    }
    if ((anchor.kind !== 'diff_line' && anchor.kind !== 'diff_range') || anchor.line === null || (anchor.side !== 'LEFT' && anchor.side !== 'RIGHT')) {
      unplaced('unsupported_anchor');
      continue;
    }
    if (!file.patchComplete) {
      unplaced('incomplete_patch');
      continue;
    }
    const startLine = anchor.startLine ?? anchor.line;
    const rowNumbers = new Set(file.hunks.flatMap((h) => h.lines.map((row) => anchor.side === 'LEFT' ? row.oldLine : row.newLine)));
    if (startLine < 1 || startLine > anchor.line || anchor.line - startLine >= rowNumbers.size || !Array.from({ length: anchor.line - startLine + 1 }, (_, i) => startLine + i).every((n) => rowNumbers.has(n))) {
      unplaced('missing_row');
      continue;
    }
    result.inline.push({ ...callout, fileId: file.id, side: anchor.side, line: anchor.line, startLine });
  }
  return result;
}
