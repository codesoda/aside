import type { GithubComment } from './generated/GithubComment';
import type { FeedbackThread, NativeFeedbackRecord } from './api';

export function object(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

export function value(record: NativeFeedbackRecord): Record<string, unknown> {
  // The records endpoint carries the native identity alongside its raw imported payload.
  const raw = record.raw ?? record.rawJson ?? record.data;
  if (typeof raw === 'string') {
    try { return object(JSON.parse(raw)); } catch { return {}; }
  }
  return object(raw ?? record);
}

export function label(value: unknown): string {
  return typeof value === 'string' && value.trim() ? value : 'Unknown';
}

export function sourceKind(record: NativeFeedbackRecord): string {
  // Imported check resource kinds include a head SHA; grouping by it would
  // create a separate section for every historical run.
  const kind = label(record.resourceKind).split(':')[0];
  return ({ reviews: 'Reviews', issue_comments: 'PR comments', check_runs: 'Check runs',
    check_annotations: 'Annotations', commit_statuses: 'Commit statuses' } as Record<string, string>)[kind] ?? kind;
}

export function safeLink(url: unknown): string | null {
  if (typeof url !== 'string') return null;
  try { return new URL(url).protocol === 'https:' ? url : null; } catch { return null; }
}

export function threadLink(thread: FeedbackThread): string | null {
  return safeLink(thread.github?.url ?? thread.github?.comments[0]?.url);
}

export function nativeComment(thread: FeedbackThread, comment: FeedbackThread['comments'][number]): GithubComment | undefined {
  return thread.github?.comments.find((native) =>
    (comment.githubNodeId && native.nodeId === comment.githubNodeId) ||
    (native.databaseId != null && comment.githubCommentId != null && String(native.databaseId) === String(comment.githubCommentId)));
}

export function commentLink(thread: FeedbackThread, comment: FeedbackThread['comments'][number]): string | null {
  return safeLink(nativeComment(thread, comment)?.url);
}

export function resolutionKnown(thread: FeedbackThread): boolean {
  return thread.resolutionKnown === true && thread.github != null && !thread.github.restOnly;
}

export function searchableThread(thread: FeedbackThread): string {
  return [thread.path, thread.githubThreadId, ...thread.comments.flatMap((comment) => [comment.author, comment.body])].join(' ').toLowerCase();
}

export function searchableRecord(record: NativeFeedbackRecord): string {
  return JSON.stringify(record).toLowerCase();
}

export function unresolvedByPath(threads: FeedbackThread[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const thread of threads) {
    if (!resolutionKnown(thread) || thread.github!.isResolved || !thread.path) continue;
    counts.set(thread.path, (counts.get(thread.path) ?? 0) + 1);
  }
  return counts;
}

export function link(record: NativeFeedbackRecord): string | null {
  const raw = value(record);
  const url = raw.url ?? raw.htmlUrl ?? raw.detailsUrl ?? raw.targetUrl ?? record.url ?? object(record.native).externalUrl;
  return safeLink(url);
}

export function nativeText(record: NativeFeedbackRecord, ...keys: string[]): string {
  const raw = value(record);
  for (const key of keys) {
    const item = raw[key] ?? record[key];
    if (typeof item === 'string' && item.trim()) return item;
    if (typeof item === 'number') return String(item);
  }
  return 'Unknown';
}

export function anchorText(record: NativeFeedbackRecord): string {
  const raw = value(record);
  const path = raw.path;
  const start = raw.startLine;
  const end = raw.endLine ?? raw.line ?? raw.originalLine;
  if (typeof path === 'string' && path) {
    const range = typeof start === 'number' && typeof end === 'number' && start !== end ? `:${start}–${end}` :
      typeof end === 'number' ? `:${end}` : ' · line unknown';
    return `${path}${range}`;
  }
  return 'Unknown / unmappable anchor';
}
