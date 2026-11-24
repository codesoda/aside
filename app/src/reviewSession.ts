import { fetchSession, watchSession, type SessionView } from './api';

/** Server snapshots are authoritative; sequence is monotonic within one session. */
export function acceptsSession(current: SessionView | null, incoming: SessionView): boolean {
  return !current || (current.sessionId === incoming.sessionId && incoming.sequence >= current.sequence);
}

/** Owns the live subscription and guards async action refreshes against newer SSE views. */
export function createReviewSession(
  onSession: (view: SessionView) => void,
  onError: (message: string) => void,
  dependencies: { watch: typeof watchSession; fetch: typeof fetchSession } = { watch: watchSession, fetch: fetchSession },
) {
  let latest: SessionView | null = null;
  let active = true;
  let blocked = false;
  let generation = 0;
  function receive(view: SessionView) {
    if (!active || blocked) return false;
    if (latest && latest.sessionId !== view.sessionId) {
      blocked = true;
      onError('The review session changed. Reload this page to continue safely.');
      return false;
    }
    if (!acceptsSession(latest, view)) return true;
    latest = view;
    onSession(view);
    onError('');
    return true;
  }
  const stop = dependencies.watch(receive, (error) => {
    if (active && !blocked) onError(String(error));
  });
  return {
    get current() { return latest; },
    get blocked() { return blocked; },
    receive,
    async refresh() {
      if (!active || blocked) return;
      const request = ++generation;
      try {
        const view = await dependencies.fetch();
        if (active && request === generation) receive(view);
      } catch (error) {
        if (active && !blocked && request === generation) onError(String(error));
      }
    },
    stop() { active = false; generation++; stop(); },
  };
}

/** Selection is transient; the persisted file id only remains valid in the current snapshot. */
export function selectedFile(session: SessionView, id: string | null) {
  return session.files.find((file) => file.id === id) ?? session.files[0];
}
export function isCurrentFile(session: SessionView, fileId: string, digest: string): boolean {
  return session.files.some((file) => file.id === fileId && file.diffDigest === digest);
}
