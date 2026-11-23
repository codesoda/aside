import { ApiError } from './api';
import type { SessionView } from './generated/SessionView';

export interface ExplainSelection {
  sessionId: string;
  baseSha: string;
  headSha: string;
  fileId: string;
  diffDigest: string;
  hunkId: string;
  lineId: string | null;
}
export interface ExplainTarget {
  selection: ExplainSelection;
  path: string;
  label: string;
}
export interface ExplainProvider {
  id: string;
  label: string;
  model: string;
  processing: 'local' | 'hosted';
  available: boolean;
  detail: string;
}
export interface ExplainTurn { question: string; answer: string }
export interface ExplainReply { requestId: string; selection: ExplainSelection; provider: string; answer: string }

export function selectionCurrent(selection: ExplainSelection, session: SessionView, fileId?: string): boolean {
  return selection.sessionId === session.sessionId && selection.headSha === session.pr.headSha
    && selection.baseSha === session.pr.baseSha && (fileId === undefined || fileId === selection.fileId)
    && session.files.some((f) => f.id === selection.fileId && f.diffDigest === selection.diffDigest
      && f.hunks.some((h) => h.id === selection.hunkId && (selection.lineId === null || h.lines.some((l) => l.id === selection.lineId))));
}

export function sameSelection(a: ExplainSelection, b: ExplainSelection): boolean {
  const keys: (keyof ExplainSelection)[] = ['sessionId', 'baseSha', 'headSha', 'fileId', 'diffDigest', 'hunkId', 'lineId'];
  return !!a && keys.every((key) => a[key] === b[key]);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, init);
  if (!response.ok) {
    const payload = await response.json().catch(() => ({}));
    throw new ApiError(response.status, payload.message ?? `Explanation request failed (${response.status}).`, payload.error ?? '');
  }
  return response.json() as Promise<T>;
}
export function explanationProviders(signal: AbortSignal): Promise<{ providers: ExplainProvider[] }> {
  return request('/api/v1/explain/providers', { signal });
}
export function askExplanation(requestId: string, provider: string, selection: ExplainSelection, question: string, history: ExplainTurn[], signal: AbortSignal): Promise<ExplainReply> {
  return request('/api/v1/explain/questions', { method: 'POST', signal,
    headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ requestId, provider, selection, question, history }) });
}
export async function cancelExplanation(requestId: string): Promise<void> {
  await request(`/api/v1/explain/questions/${encodeURIComponent(requestId)}/cancel`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
}
