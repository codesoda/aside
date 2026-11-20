import type { DiffLine } from './generated/DiffLine';
import type { ReviewThread } from './generated/ReviewThread';

/** Only current anchors on the matching side of this diff row get a marker. */
export function threadsForLine(line: DiffLine, threads: ReviewThread[]): ReviewThread[] {
  if (line.kind === 'meta') return [];
  return threads.filter((thread) => thread.anchorState === 'current' && thread.line !== null && (
    thread.side === 'LEFT' ? thread.line === line.oldLine : thread.line === line.newLine
  ));
}

export function orderThreads(threads: ReviewThread[]): ReviewThread[] {
  return [...threads].sort((a, b) => (a.line ?? Infinity) - (b.line ?? Infinity));
}
