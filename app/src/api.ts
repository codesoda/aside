import type { Evidence } from './generated/Evidence';
import type { FindingItem } from './generated/FindingItem';
import type { Page } from './generated/Page';
import type { QueueView } from './generated/QueueView';
import type { RuleAction } from './generated/RuleAction';
import type { SessionView } from './generated/SessionView';

export type { Evidence, FindingItem, QueueView, SessionView };

export class ApiError extends Error {
  // Keep explicit fields: node:test type stripping rejects parameter properties.
  status: number;
  code: string;

  constructor(status: number, message: string, code = '') {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function mutate(path: string, method: 'POST' | 'DELETE', body: object): Promise<void> {
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

export function replyToThread(threadId: string, body: string): Promise<void> {
  return mutate(`/api/v1/threads/${encodeURIComponent(threadId)}/replies`, 'POST', { body });
}

export function createFileRule(action: RuleAction, path: string): Promise<void> {
  // Later rules win, so appending an include re-includes a file excluded by
  // a folder or glob rule without editing that rule. Rules feed presentation
  // only: the server re-ranks and never reruns analysis for them.
  return mutate('/api/v1/repositories/current/exclusions', 'POST', { action, kind: 'file', expression: path });
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
    'thread.replied', 'thread.changed', 'comment.changed', 'comment.deleted',
    'review.finished', 'review.published', 'pr.refreshed',
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
