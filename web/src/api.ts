// Mirrors the Rust `SessionSnapshot` type in src/model.rs.
export type ChangeKind = 'added' | 'modified' | 'removed' | 'renamed';

export interface ChangedFile {
  id: string;
  path: string;
  previousPath: string | null;
  changeKind: ChangeKind;
  additions: number;
  deletions: number;
}

export interface PullRequest {
  owner: string;
  repo: string;
  number: number;
  title: string;
  author: string;
  url: string;
  baseRef: string;
  headRef: string;
  headSha: string;
}

export interface SessionSnapshot {
  schemaVersion: number;
  sessionId: string;
  demo: boolean;
  pr: PullRequest;
  files: ChangedFile[];
}

export async function fetchSession(): Promise<SessionSnapshot> {
  const res = await fetch('/api/v1/session');
  if (!res.ok) throw new Error(`GET /api/v1/session failed: ${res.status}`);
  return res.json();
}
