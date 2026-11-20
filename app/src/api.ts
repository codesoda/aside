import type { SessionView } from './generated/SessionView';

export type { SessionView };

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
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

export function createThread(fileId: string, diffDigest: string, side: 'LEFT' | 'RIGHT', line: number, body: string): Promise<void> {
  return mutate('/api/v1/threads', 'POST', { fileId, diffDigest, side, line, body });
}

export function replyToThread(threadId: string, body: string): Promise<void> {
  return mutate(`/api/v1/threads/${encodeURIComponent(threadId)}/replies`, 'POST', { body });
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
