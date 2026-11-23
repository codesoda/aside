import type { Evidence } from './generated/Evidence';
import type { FindingItem } from './generated/FindingItem';
import type { Page } from './generated/Page';
import type { PublishBlocker } from './generated/PublishBlocker';
import type { PublishOutcome } from './generated/PublishOutcome';
import type { PublishPlan } from './generated/PublishPlan';
import type { ReviewState } from './generated/ReviewState';
import type { QueueView } from './generated/QueueView';
import type { RuleAction } from './generated/RuleAction';
import type { RuleKind } from './generated/RuleKind';
import type { SessionView } from './generated/SessionView';
import type { Verdict } from './generated/Verdict';
import type { ReviewThread } from './generated/ReviewThread';
import type { GithubThread } from './generated/GithubThread';

export type { Evidence, FindingItem, PublishBlocker, PublishOutcome, PublishPlan, QueueView, SessionView, Verdict };

export type ConversationThread = ReviewThread;

export class ApiError extends Error {
  // Keep explicit fields: node:test type stripping rejects parameter properties.
  status: number;
  code: string;
  details: unknown;

  constructor(status: number, message: string, code = '', details?: unknown) {
    super(message);
    this.status = status;
    this.code = code;
    this.details = details;
  }
}

export type SelectionResult = { selected: boolean; commentIds: string[]; changed: string[]; selectionRevision: number; sequence: number };
export type PublishPreview = { plan: PublishPlan; blockers: PublishBlocker[]; sequence: number };

async function postJson<T>(path: string, body: object): Promise<T> {
  const res = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  const payload = await res.json().catch(() => null);
  if (!res.ok) throw new ApiError(res.status, payload?.message ?? `POST ${path} failed: ${res.status}`, payload?.error ?? '', payload?.details);
  return payload as T;
}

export function selectDrafts(commentIds: string[], selected: boolean): Promise<SelectionResult> {
  return postJson('/api/v1/review/publish/selection', { commentIds, selected });
}
export function finishReview(verdict: Verdict, summary: string): Promise<{ review: ReviewState; sequence: number }> {
  return postJson('/api/v1/review/finish', { verdict, summary });
}
export function previewPublication(verdict: Verdict, body: string): Promise<PublishPreview> {
  return postJson('/api/v1/review/publish/preview', { verdict, body });
}
export function confirmPublication(planId: string, verdict: Verdict, body: string): Promise<{ outcome: PublishOutcome; sequence: number }> {
  return postJson('/api/v1/review/publish', { planId, verdict, body, confirm: true });
}
export function refreshReview(): Promise<{ session: SessionView; sequence: number }> {
  return postJson('/api/v1/refresh', {});
}

async function mutate(path: string, method: 'POST' | 'DELETE' | 'PATCH' | 'PUT', body: object): Promise<void> {
  const res = await fetch(path, {
    method,
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    let message = `${method} ${path} failed: ${res.status}`;
    try {
      const payload = await res.json() as { error?: { message?: string }; message?: string };
      message = payload.error?.message ?? payload.message ?? message;
    } catch { /* Keep the HTTP status when the response is not JSON. */ }
    throw new ApiError(res.status, message);
  }
}

export function setReviewed(fileId: string, diffDigest: string, reviewed: boolean): Promise<void> {
  return mutate(`/api/v1/files/${encodeURIComponent(fileId)}/reviewed`, reviewed ? 'POST' : 'DELETE', { diffDigest });
}

export function createThread(fileId: string, diffDigest: string, side: 'LEFT' | 'RIGHT', line: number, body: string, startLine?: number): Promise<void> {
  return mutate('/api/v1/threads', 'POST', { fileId, diffDigest, side, line, body, ...(startLine === undefined ? {} : { startLine }) });
}

export function replyToThread(threadId: string, body: string, inReplyTo?: string): Promise<void> {
  return mutate(`/api/v1/threads/${encodeURIComponent(threadId)}/replies`, 'POST', { body, ...(inReplyTo ? { inReplyTo } : {}) });
}

export function createInsightThread(fileId: string, diffDigest: string, findingId: string, side: 'LEFT' | 'RIGHT', line?: number, startLine?: number): Promise<void> {
  return mutate('/api/v1/threads', 'POST', { subject: line === undefined ? 'file' : 'line', fileId, diffDigest, findingId, side, ...(line === undefined ? {} : { line }), ...(startLine === undefined ? {} : { startLine }) });
}

export function patchThread(threadId: string, change: { resolved?: boolean; readThrough?: string }): Promise<void> {
  return mutate(`/api/v1/threads/${encodeURIComponent(threadId)}`, 'PATCH', change);
}

export function saveReviewComment(threadId: string, body: string, sourceCommentId?: string): Promise<void> {
  return mutate(`/api/v1/threads/${encodeURIComponent(threadId)}/review-comment`, 'PUT', { body, ...(sourceCommentId ? { sourceCommentId } : {}) });
}

export function deleteReviewComment(threadId: string): Promise<void> {
  return mutate(`/api/v1/threads/${encodeURIComponent(threadId)}/review-comment`, 'DELETE', {});
}

export function retryAgent(threadId: string, commentId: string): Promise<void> {
  return mutate(`/api/v1/threads/${encodeURIComponent(threadId)}/agent-retry`, 'POST', { requestId: commentId });
}

export type ExtensionPreference = { category: string; action: 'include' | 'exclude'; enabled: boolean };
export function fetchExtensionPreferences(): Promise<{ rules: ExtensionPreference[] }> {
  return getJson('/api/v1/repositories/current/extension-preferences');
}
export function putExtensionPreference(category: string, mode: 'include' | 'exclude' | 'default'): Promise<void> {
  return mutate(`/api/v1/repositories/current/extension-preferences/${encodeURIComponent(category)}`, 'PUT', { mode });
}

export async function previewReview(input: { verdict: string; body: string }) {
  return postJson<{ plan: { planId: string; verdict: 'APPROVE' | 'REQUEST_CHANGES' | 'COMMENT'; summary: string; inline: Array<{ commentId: string; threadId: string; path: string; side: 'LEFT' | 'RIGHT'; line: number; startLine: number | null; body: string }>; replies: Array<{ commentId: string; threadId: string; parentCommentId: string; githubParentId: number | null; body: string }>; omitted: Array<{ commentId: string; threadId: string; reason: string }>; preparedCount: number; openCount: number; resolvedCount: number; headSha: string; baseSha: string; attemptOrdinal: number }; sequence: number }>('/api/v1/review/publish/preview', input);
}
export async function submitReview(input: { planId: string; verdict: string; body: string; confirm: true }) {
  return postJson<{ outcome: { attemptId: string; status: string; githubReviewId: number | null; publishedCommentIds: string[]; remainingCommentIds: string[]; error: string | null }; sequence: number }>('/api/v1/review/publish', input);
}
export type ActivePublication = { attemptId: string; affectedDraftIds: string[] };
export function fetchPublicationStatus(): Promise<{ active: ActivePublication | null }> {
  return getJson('/api/v1/review/publish/status');
}
export function reconcilePublication(input: { attemptId: string; outcome: 'posted' | 'not_posted'; confirm: true }): Promise<{ sequence: number }> {
  return postJson('/api/v1/review/publish/reconcile', input);
}
export function createScopeRule(action: RuleAction, kind: RuleKind, expression: string, lifetime: 'review' | 'repository'): Promise<void> {
  return mutate('/api/v1/repositories/current/exclusions', 'POST', { action, kind, expression, lifetime });
}

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(path);
  if (!res.ok) {
    let code = '';
    let message = `GET ${path} failed: ${res.status}`;
    try {
      const payload = await res.json() as { error?: string; message?: string };
      code = payload.error ?? '';
      message = payload.message ?? message;
    } catch { /* Keep the HTTP status when the response is not JSON. */ }
    throw new ApiError(res.status, message, code);
  }
  return (await res.json()) as T;
}

export function fetchQueue(): Promise<QueueView> {
  return getJson<QueueView>('/api/v1/analysis/queue');
}

/** Pagination gave up because results kept changing during a live run. */
export class ResultsChangedError extends Error {
  constructor() {
    super('Analysis results kept changing while loading; they will reload on the next update.');
  }
}

/**
 * Reads every page of a list. Cursors are keyset cursors (#64): they survive
 * findings arriving mid-walk, so a live run no longer forces restarts. A 409
 * `cursor_stale` now means a genuine change — the snapshot moved, or something
 * in the already-paged region changed — and the recovery is the server's
 * documented contract: restart from the first page rather than splice pages
 * from different states. (This was #38's bounded-restart workaround; the
 * server-side design it papered over is fixed.)
 */
export async function fetchAllPages<T>(path: string, attempts = 3): Promise<{ items: T[]; last: Page<T> }> {
  for (let attempt = 0; attempt < attempts; attempt++) {
    const items: T[] = [];
    let cursor: string | null = null;
    try {
      for (;;) {
        const sep = path.includes('?') ? '&' : '?';
        const page: Page<T> = await getJson<Page<T>>(`${path}${sep}limit=200${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`);
        items.push(...page.items);
        if (!page.hasMore || !page.nextCursor) return { items, last: page };
        cursor = page.nextCursor;
      }
    } catch (error) {
      if (error instanceof ApiError && error.status === 409 && error.code === 'cursor_stale') continue;
      throw error;
    }
  }
  throw new ResultsChangedError();
}

/** Native GitHub feedback is read-only and never a finding. */
export type NativeFeedbackRecord = {
  resourceKind: string;
  native?: Record<string, unknown>;
  [field: string]: unknown;
};

export type FeedbackThread = Omit<ReviewThread, 'comments'> & {
  /** Persisted association to native GitHub comments, independent of database ID. */
  comments: (ReviewThread['comments'][number] & { githubNodeId?: string })[];
  /** Joined from the current GitHub snapshot, not persisted review state. */
  github: GithubThread | null;
  resolutionKnown: boolean;
};

export function fetchFeedbackThreads(): Promise<{ items: FeedbackThread[]; last: Page<FeedbackThread> }> {
  return fetchAllPages<FeedbackThread>('/api/v1/github-feedback/threads?includeIgnored=true');
}

export function fetchFeedbackRecords(): Promise<{ items: NativeFeedbackRecord[]; last: Page<NativeFeedbackRecord> }> {
  return fetchAllPages<NativeFeedbackRecord>('/api/v1/github-feedback/records?includeIgnored=true');
}

/** Current findings, including ignored files so they can be revealed. */
export function fetchFindings(): Promise<{ items: FindingItem[]; last: Page<FindingItem> }> {
  return fetchAllPages<FindingItem>('/api/v1/analysis/findings?includeIgnored=true');
}

export async function fetchEvidence(findingId: string): Promise<Evidence[]> {
  const { items } = await fetchAllPages<Evidence>(`/api/v1/analysis/findings/${encodeURIComponent(findingId)}/evidence?includeIgnored=true`);
  return items;
}

export async function fetchSession(): Promise<SessionView> {
  const res = await fetch('/api/v1/session');
  if (!res.ok) throw new Error(`GET /api/v1/session failed: ${res.status}`);
  return (await res.json()) as SessionView;
}

// Events are notifications, not patches: always reload the authoritative view.
export function watchSession(
  onUpdate: (session: SessionView) => void,
  onError: (error: unknown) => void,
): () => void {
  const source = new EventSource('/api/v1/events');
  let active = true;
  let request = 0;
  async function refresh() {
    const current = ++request;
    try {
      const session = await fetchSession();
      if (active && current === request) onUpdate(session);
    } catch (error) {
      if (active && current === request) onError(error);
    }
  }

  source.onopen = refresh; // Reconnects and missed events both require a resync.
  for (const event of [
    'resync', 'exclusions.changed', 'file.reviewed', 'thread.created',
    'thread.replied', 'thread.changed', 'comment.changed', 'comment.deleted', 'agent.presence',
    'review.finished', 'review.published', 'review.selection.changed', 'pr.refreshed',
    // Run lifecycle (#27): small versioned deltas; the session view carries
    // the authoritative per-source coverage.
    'analysis.run.queued', 'analysis.run.started', 'analysis.run.succeeded',
    'analysis.run.partial', 'analysis.run.skipped', 'analysis.run.cancelled',
    'analysis.run.failed', 'analysis.run.stale', 'analysis.findings.upserted',
    'analysis.findings.invalidated',
  ]) {
    source.addEventListener(event, refresh);
  }
  void refresh();
  return () => {
    active = false;
    source.close();
  };
}
