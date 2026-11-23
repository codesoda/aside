import type { DiffLine } from './generated/DiffLine';
import type { ConversationThread } from './api';

/** Only current anchors on the matching side of this diff row get a marker. */
export function threadsForLine(line: DiffLine, threads: ConversationThread[]): ConversationThread[] {
  if (line.kind === 'meta') return [];
  return threads.filter((thread) => thread.anchorState === 'current' && thread.line !== null && (
    thread.side === 'LEFT' ? (thread.startLine ?? thread.line) === line.oldLine : (thread.startLine ?? thread.line) === line.newLine
  ));
}

export function orderThreads(threads: ConversationThread[]): ConversationThread[] {
  return [...threads].sort((a, b) => (a.startLine ?? a.line ?? Infinity) - (b.startLine ?? b.line ?? Infinity));
}
