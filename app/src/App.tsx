import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import {
  ApiError, createInsightThread, createScopeRule, createThread, fetchExtensionPreferences, fetchFeedbackRecords, fetchFeedbackThreads,
  fetchFindings, fetchQueue, patchThread, promoteFinding, putExtensionPreference, selectDrafts, setFindingState,
  setReviewed, type ConversationThread, type FeedbackThread, type FindingItem, type NativeFeedbackRecord,
  type QueueView, type SessionView,
} from './api';
import type { Page } from './generated/Page';
import type { DiffHunk } from './generated/DiffHunk';
import type { DiffLine } from './generated/DiffLine';
import type { Side } from './generated/Side';
import { placeCallouts } from './callouts';
import { nextUnreviewedFile } from './fileTree';
import { reviewPageTitle } from './pageTitle';
import { createReviewSession, selectedFile } from './reviewSession';
import { selectionCurrent, type ExplainTarget } from './explain';
import { DiffView, lineSide } from './diff/DiffView';
import ConversationPanel, { type ConversationDraft } from './workspace/ConversationPanel';
import ReviewPrep from './workspace/ReviewPrep';
import FindingBrowser from './insights/FindingBrowser';
import FileNavigator from './workspace/FileNavigator';
import FinishReview from './workspace/FinishReview';
import Feedback from './workspace/Feedback';
import Conversations from './workspace/Conversations';
import ExplainPanel from './ExplainPanel';
import { Button } from './components/ui/button';

const views = [
  { id: 'prep', label: 'Review prep' }, { id: 'files', label: 'Files changed' },
  { id: 'conversations', label: 'Conversations' }, { id: 'feedback', label: 'GitHub feedback' },
  { id: 'finish', label: 'Finish review' },
] as const;
type View = (typeof views)[number]['id'];
type Anchor = { fileId: string; digest: string; side: Side; line: number; startLine?: number };
type LineDraft = { anchor: Anchor; sessionId: string; path: string; text: string };
const draftKey = (sessionId: string, anchor: Anchor) => JSON.stringify([sessionId, anchor.fileId, anchor.digest, anchor.side, anchor.startLine ?? anchor.line, anchor.line]);
type ScopeTarget = { kind: 'file' | 'directory' | 'glob'; expression: string; action: 'include' | 'exclude'; lifetime: 'review' | 'repository' };
const message = (failure: unknown) => failure instanceof Error ? failure.message : String(failure);

export default function App() {
  const [session, setSession] = useState<SessionView | null>(null);
  const [error, setError] = useState('');
  const [actionError, setActionError] = useState('');
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeView, setActiveView] = useState<View>('prep');
  const [selectedThreadId, setSelectedThreadId] = useState<string | null>(null);
  const [selectedFindingId, setSelectedFindingId] = useState<string | null>(null);
  const [lineDrafts, setLineDrafts] = useState<Record<string, LineDraft>>({});
  const [activeDrafts, setActiveDrafts] = useState<Record<string, string>>({});
  const [rangeCandidate, setRangeCandidate] = useState<Anchor | null>(null);
  const file = session ? selectedFile(session, selectedId) : undefined;
  const activeDraftKey = file ? activeDrafts[file.id] : undefined;
  const activeDraft = activeDraftKey ? lineDrafts[activeDraftKey] : undefined;
  const anchor = activeDraft?.anchor ?? null;
  const draft = activeDraft?.text ?? '';
  const [conversationTop, setConversationTop] = useState(0);
  const drafts = useRef<Record<string, ConversationDraft>>({});
  const pendingConversationActions = useRef(new Set<string>());
  const [, setConversationVersion] = useState(0);
  const [explainTarget, setExplainTarget] = useState<ExplainTarget | null>(null);
  const explainTrigger = useRef<HTMLElement | null>(null);
  const [queue, setQueue] = useState<QueueView | null>(null);
  const [findings, setFindings] = useState<FindingItem[] | null>(null);
  const [snapshotId, setSnapshotId] = useState<string | null>(null);
  const [insightsLoading, setInsightsLoading] = useState(false);
  const [insightsError, setInsightsError] = useState<string | null>(null);
  const [findingsLoadError, setFindingsLoadError] = useState<string | null>(null);
  const [historical, setHistorical] = useState<FindingItem[] | null>(null);
  const [historicalSnapshot, setHistoricalSnapshot] = useState<string | null>(null);
  const [historicalLoading, setHistoricalLoading] = useState(false);
  const [historicalError, setHistoricalError] = useState<string | null>(null);
  const [findingPending, setFindingPending] = useState<string | null>(null);
  const [findingError, setFindingError] = useState<string | null>(null);
  const promotions = useRef(new Map<string, { id: string; revision: number }>());
  const [feedbackThreads, setFeedbackThreads] = useState<FeedbackThread[]>([]);
  const [feedbackRecords, setFeedbackRecords] = useState<NativeFeedbackRecord[]>([]);
  const [feedbackThreadPage, setFeedbackThreadPage] = useState<Page<FeedbackThread> | null>(null);
  const [feedbackRecordPage, setFeedbackRecordPage] = useState<Page<NativeFeedbackRecord> | null>(null);
  const [feedbackLoading, setFeedbackLoading] = useState(false);
  const [feedbackErrors, setFeedbackErrors] = useState<string[]>([]);
  const [scopeError, setScopeError] = useState<string | null>(null);
  const [lastScope, setLastScope] = useState<ScopeTarget | null>(null);
  const [preferences, setPreferences] = useState<Record<string, 'include' | 'exclude'>>({});
  const [headerMenu, setHeaderMenu] = useState(false);
  const controller = useRef<ReturnType<typeof createReviewSession> | null>(null);
  const mounted = useRef(false);
  const requests = useRef({ insights: 0, historical: 0, feedback: 0, preferences: 0 });
  const latest = useRef({ session, selectedId, selectedThreadId, selectedFindingId, activeView, anchor, draft, findings });
  latest.current = { session, selectedId, selectedThreadId, selectedFindingId, activeView, anchor, draft, findings };
  const current = () => controller.current?.current ?? latest.current.session;
  const refresh = async () => { await controller.current?.refresh(); };

  function loadInsights() {
    const request = ++requests.current.insights;
    setInsightsLoading(true);
    void Promise.allSettled([fetchQueue(), fetchFindings()]).then(([q, f]) => {
      if (!mounted.current || request !== requests.current.insights) return;
      setInsightsLoading(false);
      if (q.status === 'fulfilled') setQueue(q.value);
      if (f.status === 'fulfilled') { setFindings(f.value.items); setSnapshotId(f.value.last.snapshotId); setFindingsLoadError(null); }
      else setFindingsLoadError(message(f.reason));
      const failed = [q, f].find((result) => result.status === 'rejected') as PromiseRejectedResult | undefined;
      setInsightsError(failed ? message(failed.reason) : null);
    });
  }
  function loadHistorical() {
    const request = ++requests.current.historical;
    setHistoricalLoading(true); setHistoricalError(null);
    void fetchFindings(true).then((result) => {
      if (!mounted.current || request !== requests.current.historical) return;
      setHistorical(result.items.filter((item) => item.label === 'historical'));
      setHistoricalSnapshot(result.last.snapshotId);
    }).catch((failure) => { if (mounted.current && request === requests.current.historical) setHistoricalError(message(failure)); })
      .finally(() => { if (mounted.current && request === requests.current.historical) setHistoricalLoading(false); });
  }
  function loadFeedback() {
    const request = ++requests.current.feedback;
    setFeedbackLoading(true); setFeedbackErrors([]);
    void Promise.allSettled([fetchFeedbackThreads(), fetchFeedbackRecords()]).then(([threads, records]) => {
      if (!mounted.current || request !== requests.current.feedback) return;
      setFeedbackLoading(false);
      setFeedbackThreads(threads.status === 'fulfilled' ? threads.value.items : []);
      setFeedbackThreadPage(threads.status === 'fulfilled' ? threads.value.last : null);
      setFeedbackRecords(records.status === 'fulfilled' ? records.value.items : []);
      setFeedbackRecordPage(records.status === 'fulfilled' ? records.value.last : null);
      setFeedbackErrors([threads, records].filter((result) => result.status === 'rejected').map((result) => message((result as PromiseRejectedResult).reason)));
    });
  }
  function loadPreferences() {
    const request = ++requests.current.preferences;
    void fetchExtensionPreferences().then(({ rules }) => {
      if (mounted.current && request === requests.current.preferences)
        setPreferences(Object.fromEntries(rules.filter((rule) => rule.enabled).map((rule) => [rule.category, rule.action])));
    }).catch((failure) => { if (mounted.current && request === requests.current.preferences) setScopeError(message(failure)); });
  }
  useEffect(() => {
    mounted.current = true;
    const live = createReviewSession((view) => { setSession(view); loadInsights(); loadFeedback(); loadPreferences(); }, setError);
    controller.current = live;
    const onVisible = () => { if (!document.hidden) loadInsights(); };
    document.addEventListener('visibilitychange', onVisible);
    return () => { mounted.current = false; Object.keys(requests.current).forEach((key) => { requests.current[key as keyof typeof requests.current]++; }); live.stop(); controller.current = null; document.removeEventListener('visibilitychange', onVisible); };
  }, []);
  useEffect(() => { document.title = error ? 'Review unavailable · Aside' : session ? reviewPageTitle(session.pr) : 'Loading review… · Aside'; }, [error, session]);
  useEffect(() => {
    const next = (findings ?? []).filter((item) => item.disposition === 'snoozed' && item.userState?.snoozedUntil)
      .map((item) => Date.parse(item.userState!.snoozedUntil!)).filter(Number.isFinite).sort((a, b) => a - b)[0];
    if (next === undefined) return;
    const timer = setTimeout(loadInsights, Math.min(Math.max(1000, next - Date.now() + 100), 2147483647));
    return () => clearTimeout(timer);
  }, [findings]);

  const fileState = session?.fileStates.find((entry) => entry.fileId === file?.id);
  const allThreads = (session?.threads ?? []) as ConversationThread[];
  const threads = allThreads.filter((thread) => thread.fileId === file?.id || thread.path === file?.path);
  const sortedThreads = [...threads].sort((a, b) => (a.line ?? -1) - (b.line ?? -1));
  const selectedThread = allThreads.find((thread) => thread.id === selectedThreadId);
  const currentFindings = snapshotId === session?.analysis.snapshotId ? findings?.filter((item) => item.label === 'current') ?? null : null;
  const fileFindings = (findings ?? []).filter((item) => item.finding.anchor.fileId === file?.id || item.finding.anchor.currentPath === file?.path);
  const callouts = placeCallouts(fileFindings, file ? [file] : [], session?.analysis.snapshotId ?? '', 'all', session?.threads ?? []);
  const unthreaded = (item: FindingItem) => !allThreads.some((thread) => thread.findingId === item.finding.id);
  const headerThreads = threads.filter((thread) => thread.subject === 'file' && thread.anchorState === 'current');
  const unplacedThreads = threads.filter((thread) => thread.anchorState !== 'current' || (thread.subject !== 'file' && thread.line === null));
  const headerFindings = callouts.file.filter((entry) => unthreaded(entry.item)).map((entry) => entry.item);
  const unplacedFindings = callouts.unplaced.filter((entry) => unthreaded(entry.item)).map((entry) => entry.item);
  const ignoredPaths = new Set(session?.fileStates.filter((state) => state.scope.ignored).map((state) => session.files.find((item) => item.id === state.fileId)?.path).filter((path): path is string => !!path) ?? []);
  const validAnchor = !!anchor && !!file?.patchComplete && anchor.fileId === file.id && anchor.digest === file.diffDigest && !fileState?.scope.ignored;
  const staleDrafts = Object.entries(lineDrafts).filter(([, entry]) => entry.text.trim() && (entry.sessionId !== session?.sessionId || !session.files.some((candidate) => candidate.id === entry.anchor.fileId && candidate.diffDigest === entry.anchor.digest && candidate.patchComplete)));
  const otherDrafts = Object.entries(lineDrafts).filter(([key, entry]) => entry.text.trim() && key !== activeDraftKey && entry.anchor.fileId === file?.id && entry.anchor.digest === file.diffDigest);
  useLayoutEffect(() => {
    const diff = document.getElementById('file-review');
    const marker = selectedThreadId && diff && [...diff.querySelectorAll<HTMLElement>('[data-thread-anchor]')].find((row) => row.dataset.threadAnchor?.split(' ').includes(selectedThreadId));
    const align = () => {
      const code = marker ? marker.querySelector('[data-code-line]') ?? marker : null;
      setConversationTop(diff && code ? Math.max(0, code.getBoundingClientRect().top - diff.getBoundingClientRect().top) : 0);
    };
    align();
    window.addEventListener('resize', align);
    const observer = typeof ResizeObserver !== 'undefined' && diff ? new ResizeObserver(align) : null;
    if (observer && diff) observer.observe(diff);
    return () => { window.removeEventListener('resize', align); observer?.disconnect(); };
  }, [selectedThreadId, file?.id, file?.diffDigest, activeView]);

  function selectFile(id: string) {
    setSelectedId(id); setHeaderMenu(false); setSelectedThreadId(null); setSelectedFindingId(null); setRangeCandidate(null);
    setExplainTarget(null); setActionError('');
  }
  function openReview(id?: string) {
    const moving = latest.current.activeView !== 'files' || window.matchMedia?.('(max-width: 760px)').matches;
    if (id && id !== (current() && selectedFile(current()!, latest.current.selectedId)?.id)) selectFile(id);
    setActiveView('files');
    if (moving) requestAnimationFrame(() => document.getElementById('file-review-heading')?.focus());
  }
  function openConversation(fileId: string, threadId: string) {
    openReview(fileId); void showThread(threadId);
  }
  async function showThread(id: string) {
    setSelectedThreadId(id);
    requestAnimationFrame(() => {
      const mobile = window.matchMedia?.('(max-width: 1119px)').matches;
      const card = document.getElementById(`${mobile ? 'thread-mobile-' : 'thread-'}${id}`);
      card?.focus({ preventScroll: true });
      if (mobile) card?.scrollIntoView({ block: 'nearest' });
    });
    const thread = current()?.threads.find((entry) => entry.id === id);
    const last = thread?.comments.at(-1);
    if (thread?.source === 'local' && last && thread.unreadCount) void act(() => patchThread(id, { readThrough: last.id }));
  }
  async function act(operation: () => Promise<void>, onSuccess?: () => void) {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setActionError('');
    try { await operation(); await refresh(); loadInsights(); onSuccess?.(); }
    catch (failure) {
      if (failure instanceof ApiError && failure.status === 409) {
        await refresh(); setActionError('The diff changed. Review the latest file before trying again.');
      } else setActionError(message(failure));
    } finally { busyRef.current = false; setBusy(false); }
  }
  function toggleViewed(checked: boolean) {
    if (!file || fileState?.scope.ignored) return;
    const { id, diffDigest } = file;
    void act(() => setReviewed(id, diffDigest, checked), () => {
      const view = current();
      if (!checked || !view?.files.some((entry) => entry.id === id && entry.diffDigest === diffDigest) || selectedFile(view, latest.current.selectedId)?.id !== id) return;
      const next = nextUnreviewedFile(view.files, view.fileStates, id);
      if (next) selectFile(next.id);
    });
  }
  async function setScope(target: ScopeTarget) {
    if (busyRef.current) return;
    setLastScope(target); setScopeError(null); busyRef.current = true; setBusy(true);
    try { await createScopeRule(target.action, target.kind, target.expression, target.lifetime); await refresh(); loadInsights(); }
    catch (failure) { setScopeError(message(failure)); }
    finally { busyRef.current = false; setBusy(false); }
  }
  async function setTypePreference(extension: string, mode: 'default' | 'include' | 'exclude') {
    if (busyRef.current) return;
    setScopeError(null); busyRef.current = true; setBusy(true);
    try { await putExtensionPreference(extension, mode); await refresh(); loadPreferences(); loadInsights(); }
    catch (failure) { setScopeError(message(failure)); loadPreferences(); }
    finally { busyRef.current = false; setBusy(false); }
  }
  async function toggleSelection(ids: string[], selected: boolean) {
    if (!ids.length || busyRef.current) return;
    busyRef.current = true; setBusy(true); setActionError('');
    try { await selectDrafts(ids, selected); await refresh(); }
    catch (failure) { setActionError(message(failure)); await refresh(); }
    finally { busyRef.current = false; setBusy(false); }
  }
  function openLineDraft(target: Anchor) {
    if (!session || !file?.patchComplete || fileState?.scope.ignored || target.fileId !== file.id || target.digest !== file.diffDigest) return;
    const key = draftKey(session.sessionId, target);
    setLineDrafts((all) => all[key] ? all : { ...all, [key]: { anchor: target, text: '', sessionId: session.sessionId, path: file.path } });
    setActiveDrafts((all) => ({ ...all, [target.fileId]: key }));
    setRangeCandidate(null);
    setActionError('');
    requestAnimationFrame(() => document.getElementById('draft')?.focus());
  }
  function editLineDraft(key: string, text: string) {
    setLineDrafts((all) => all[key] ? { ...all, [key]: { ...all[key], text } } : all);
  }
  function discardLineDraft(key: string, savedText?: string) {
    setLineDrafts((all) => {
      if (!all[key] || (savedText !== undefined && all[key].text !== savedText)) return all;
      const copy = { ...all };
      delete copy[key];
      return copy;
    });
  }
  function selectLine(line: DiffLine) {
    const side = lineSide(line), number = side === 'LEFT' ? line.oldLine : line.newLine;
    if (!file || !side || number === null) return;
    openLineDraft({ fileId: file.id, digest: file.diffDigest, side, line: number });
  }
  function selectCode() {
    const view = latest.current.session, target = view && selectedFile(view, latest.current.selectedId);
    if (!target?.patchComplete || view?.fileStates.find((state) => state.fileId === target.id)?.scope.ignored) return;
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || !selection.toString().trim() || !selection.rangeCount) return;
    const range = selection.getRangeAt(0), code = document.querySelector('#file-review .workspace-code');
    if (!code?.contains(range.commonAncestorContainer)) return;
    if ([range.startContainer, range.endContainer].some((node) => (node instanceof Element ? node : node.parentElement)?.closest('[data-callout-scope], .callout-band'))) return;
    const lines = [...code.querySelectorAll<HTMLElement>('[data-code-line]')].filter((element) => range.intersectsNode(element));
    if (!lines.length || lines.some((element) => !element.dataset.side || element.dataset.side !== lines[0].dataset.side)) return;
    const numbers = lines.map((element) => Number(element.dataset.codeLine));
    if (numbers.some((number) => !Number.isInteger(number)) || numbers.at(-1)! - numbers[0] !== numbers.length - 1) return;
    setRangeCandidate({ fileId: target.id, digest: target.diffDigest, side: lines[0].dataset.side as Side, line: numbers.at(-1)!, ...(numbers.length > 1 ? { startLine: numbers[0] } : {}) });
  }
  useEffect(() => {
    window.addEventListener('mouseup', selectCode); window.addEventListener('keyup', selectCode);
    return () => { window.removeEventListener('mouseup', selectCode); window.removeEventListener('keyup', selectCode); };
  }, []);
  function saveDraft() {
    const view = current();
    if (!view || !activeDraftKey || !activeDraft) return;
    const { anchor: active, text, sessionId } = activeDraft;
    const target = selectedFile(view, latest.current.selectedId);
    if (!text.trim() || sessionId !== view.sessionId || target?.id !== active.fileId || !target.patchComplete || target.diffDigest !== active.digest || view.fileStates.find((state) => state.fileId === active.fileId)?.scope.ignored) return;
    void act(() => createThread(active.fileId, active.digest, active.side, active.line, text.trim(), active.startLine), () => discardLineDraft(activeDraftKey, text));
  }
  function explainChange(hunk: DiffHunk, line?: DiffLine, trigger?: HTMLButtonElement) {
    if (!session || !file) return;
    explainTrigger.current = trigger ?? document.activeElement as HTMLElement;
    setExplainTarget({ selection: { sessionId: session.sessionId, baseSha: session.pr.baseSha, headSha: session.pr.headSha, fileId: file.id, diffDigest: file.diffDigest, hunkId: hunk.id, lineId: line?.id ?? null }, path: file.path,
      label: line ? `${line.kind === 'deletion' ? `Removed old line ${line.oldLine}` : `Added new line ${line.newLine}`} and adjacent changes` : `Change block: old lines ${hunk.oldStart}–${hunk.oldStart + Math.max(0, hunk.oldCount - 1)}, new lines ${hunk.newStart}–${hunk.newStart + Math.max(0, hunk.newCount - 1)}` });
  }
  function openMarker(groupThreads: { id: string }[], groupFindings: { id: string }[]) {
    const selected = latest.current.selectedThreadId, index = groupThreads.findIndex((thread) => thread.id === selected);
    if (index >= 0 && index + 1 < groupThreads.length) { void showThread(groupThreads[index + 1].id); return; }
    if (index < 0 && groupThreads.length && !groupFindings.some((item) => item.id === latest.current.selectedFindingId)) { void showThread(groupThreads[0].id); return; }
    const findingIndex = groupFindings.findIndex((item) => item.id === latest.current.selectedFindingId);
    const finding = groupFindings[findingIndex >= 0 ? (findingIndex + 1) % groupFindings.length : 0];
    if (finding) void showFinding(finding.id);
    else if (groupThreads.length) void showThread(groupThreads[0].id);
  }
  async function showFinding(id: string) {
    const item = (latest.current.findings ?? []).find((entry) => entry.finding.id === id);
    const view = current(), target = view?.files.find((entry) => entry.id === item?.finding.anchor.fileId || entry.path === item?.finding.anchor.currentPath);
    if (!item || !target) return;
    setSelectedFindingId(id);
    const existing = view?.threads.find((thread) => thread.findingId === id);
    if (existing) { await showThread(existing.id); return; }
    const location = item.finding.anchor, side = location.side ?? 'RIGHT', line = location.line ?? undefined;
    await act(() => createInsightThread(target.id, target.diffDigest, id, side, line, line === undefined ? undefined : location.startLine ?? undefined));
    const created = current()?.threads.find((thread) => thread.findingId === id);
    if (created) await showThread(created.id);
  }
  function openFindingDraft(thread: ConversationThread | undefined) {
    const target = current()?.files.find((entry) => entry.id === thread?.fileId || entry.path === thread?.path);
    if (!thread?.reviewComment || !target) { setFindingError('The draft is no longer in the current file view.'); return; }
    const prior = drafts.current[thread.id];
    if (!prior?.editing && !thread.reviewComment.locked) drafts.current[thread.id] = { reply: prior?.reply ?? '', outgoing: thread.reviewComment.body, editing: true };
    openConversation(target.id, thread.id);
  }
  async function changeFinding(item: FindingItem, disposition: 'untouched' | 'dismissed_local' | 'snoozed', until?: string) {
    if (findingPending) return;
    setFindingPending(item.finding.id); setFindingError(null);
    try { await setFindingState(item.finding.id, item.userState?.revision ?? 0, disposition, until); }
    catch (failure) { setFindingError(failure instanceof ApiError && failure.status === 409 ? 'Finding changed. Showing the latest state; try again.' : message(failure)); }
    finally { setFindingPending(null); loadInsights(); await refresh(); }
  }
  async function promote(item: FindingItem) {
    if (findingPending) return;
    const id = item.finding.id, operation = promotions.current.get(id) ?? { id: crypto.randomUUID(), revision: item.userState?.revision ?? 0 };
    promotions.current.set(id, operation); setFindingPending(id); setFindingError(null);
    try { const result = await promoteFinding(id, operation.revision, operation.id); await refresh(); promotions.current.delete(id); openFindingDraft(current()?.threads.find((thread) => thread.id === result.thread.id)); }
    catch (failure) {
      const rejected = failure instanceof ApiError && failure.status >= 400 && failure.status < 500 && ![408, 425, 429].includes(failure.status);
      if (rejected) promotions.current.delete(id);
      setFindingError(rejected ? message(failure) : `Promotion outcome uncertain. Retry uses the same operation ID. ${message(failure)}`);
      await refresh();
    } finally { setFindingPending(null); loadInsights(); }
  }
  function threadNavigation() {
    const index = sortedThreads.findIndex((thread) => thread.id === selectedThreadId);
    return <nav className="mb-2 flex justify-between gap-2 text-xs" aria-label="Conversation navigation"><Button disabled={index <= 0} onClick={() => void showThread(sortedThreads[index - 1].id)}>Previous</Button><Button disabled={index < 0 || index >= sortedThreads.length - 1} onClick={() => void showThread(sortedThreads[index + 1].id)}>Next</Button></nav>;
  }
  function draftSelection(thread: ConversationThread) {
    const comment = thread.reviewComment;
    return comment && <div className="workspace-panel p-2" aria-label="Publication selection"><label className="block text-xs"><input type="checkbox" disabled={busy || comment.locked} checked={comment.selected} onChange={(event) => void toggleSelection([comment.id], event.target.checked)} /> Include prepared review comment when publishing</label><p className="whitespace-pre-wrap break-words text-xs">{comment.body}</p>{comment.blocker && <p className="text-xs text-danger">{comment.blocker}</p>}</div>;
  }
  const panel = (mobile = false) => selectedThread && <>{threadNavigation()}{draftSelection(selectedThread)}<ConversationPanel mobile={mobile} thread={selectedThread} drafts={drafts.current} pendingActions={pendingConversationActions.current} onDraftChange={() => setConversationVersion((version) => version + 1)} agentConnected={session?.agentPresence?.connected ?? false} finding={fileFindings.find((item) => item.finding.id === selectedThread.findingId)} onClose={() => { setSelectedThreadId(null); setSelectedFindingId(null); }} onError={setActionError} onRefresh={refresh} /></>;
  const counts = session?.files.reduce((acc, entry) => ({ add: acc.add + entry.additions, del: acc.del + entry.deletions }), { add: 0, del: 0 }) ?? { add: 0, del: 0 };
  if (!session || controller.current?.blocked) return <div className="workspace"><header className="workspace-toolbar">Aside</header><p role={error ? 'alert' : 'status'} className="p-6">{error || 'Loading review…'}</p></div>;
  return <div className="workspace">
    <header className="workspace-toolbar"><div className="min-w-0"><p className="workspace-crumb">{session.pr.owner} / {session.pr.repo} <span>/ pull requests /</span> #{session.pr.number}</p><h1 className="workspace-title">{session.pr.title} <span>#{session.pr.number}</span></h1><p className="mt-1 text-xs text-muted">{session.pr.author} wants to merge into <code className="workspace-ref">{session.pr.baseRef}</code> from <code className="workspace-ref">{session.pr.headRef}</code></p></div><div className="flex flex-wrap items-center gap-2"><span className="workspace-status">{session.pr.isDraft ? 'Draft' : session.pr.state.toLowerCase()}</span>{session.demo && <span className="workspace-chip">Demo · local only</span>}<Button className="workspace-button" onClick={() => setActiveView('finish')}>Draft ({session.threads.filter((thread) => !!thread.reviewComment).length}) · Finish review</Button></div></header>
    <nav className="workspace-tabs" aria-label="Review workspace">{views.map((view) => <button key={view.id} type="button" className="workspace-tab" aria-pressed={activeView === view.id} aria-controls={'workspace-' + view.id} onClick={() => setActiveView(view.id)}>{view.label}{view.id === 'files' && <span className="workspace-count">{session.files.length}</span>}{view.id === 'conversations' && <span className="workspace-count">{session.threads.length + session.reviews.length + session.issueComments.length}</span>}</button>)}</nav>
    {(error || actionError || session.warnings.length > 0 || session.review.unresolvedPublication) && <div className="workspace-notices">{error && <p role="alert">Refresh failed: {error}</p>}{actionError && <p role="alert">{actionError}</p>}{session.warnings.map((warning, index) => <p key={index}>{warning}</p>)}{session.review.unresolvedPublication && <p role="alert">Publication remains unresolved. Check GitHub; Aside will not retry automatically.</p>}</div>}
    <div className="workspace-body"><aside className="workspace-sidebar" aria-label="Review file tree"><div className="sidebar-content"><div className="sidebar-heading"><h2>Changed files <span>{session.progress.inScope} / {session.files.length}</span></h2></div><FileNavigator files={session.files} states={session.fileStates} queue={queue} rules={session.rules} selectedId={file?.id} typePreferences={preferences} pending={busy} error={scopeError} onRetry={() => { if (lastScope) void setScope(lastScope); }} onSelect={openReview} onScope={(target) => void setScope(target)} onTypePreference={(extension, mode) => void setTypePreference(extension, mode)} /></div></aside>
    <main className="workspace-main">
      <section id="workspace-prep" hidden={activeView !== 'prep'} aria-label="Review prep"><ReviewPrep session={session} queue={queue} findings={currentFindings} threads={session.threads} insightsError={insightsError} onOpenFile={(id, findingId) => { openReview(id); if (findingId) requestAnimationFrame(() => void showFinding(findingId)); }} onOpenThread={openConversation} onStart={() => openReview(fileState?.scope.ignored ? session.files.find((entry) => !session.fileStates.find((state) => state.fileId === entry.id)?.scope.ignored)?.id : undefined)} /><FindingBrowser findings={snapshotId === session.analysis.snapshotId ? findings : null} loading={insightsLoading} historicalFindings={historicalSnapshot === session.analysis.snapshotId ? historical : null} historicalLoading={historicalLoading} historicalLoadError={historicalError} onHistorical={loadHistorical} onHistoricalRetry={loadHistorical} findingsWithDrafts={new Set(allThreads.flatMap((thread) => thread.findingId && thread.reviewComment ? [thread.findingId] : []))} pending={findingPending} error={findingError} loadError={findingsLoadError} onRetry={loadInsights} onState={(item, disposition, until) => void changeFinding(item, disposition, until)} onPromote={(item) => void promote(item)} onOpenDraft={(item) => openFindingDraft(allThreads.find((thread) => thread.findingId === item.finding.id))} /></section>
      <section id="workspace-files" hidden={activeView !== 'files'} aria-label="Files changed"><div className="workspace-view-heading"><div><p className="workspace-eyebrow">Files changed · Review</p><h2 id="file-review-heading" tabIndex={-1}>{fileState?.reviewStatus === 'reviewed-current' ? 'This version is already reviewed.' : 'Pick up where you left off.'}</h2><p className="workspace-subtitle">{session.progress.reviewed} of {session.progress.inScope} in-scope files reviewed · {session.progress.changedSinceReviewed} changed since review · {session.progress.ignored} ignored · <span className="text-added">+{counts.add}</span> <span className="text-deleted">−{counts.del}</span></p></div></div>
      {file ? <div className={`workspace-review-grid${selectedThread ? ' conversation-open' : ''}`}><section id="file-review" className="workspace-diff" aria-label="File review"><div className="workspace-diff-header"><div><h3 className="font-mono text-sm font-semibold">{file.path}</h3><p className="text-xs text-muted">{file.changeKind} · {fileState?.scope.ignored ? 'Ignored' : fileState?.status === 'reviewed-current' ? 'Viewed' : fileState?.status === 'changed-since-reviewed' ? 'Changed since review' : 'Unreviewed'}{file.previousPath && ` · from ${file.previousPath}`}{!file.patchComplete && ' · incomplete patch'}</p></div><div className="workspace-diff-actions"><span className="font-mono text-xs">+{file.additions} −{file.deletions}</span><Button aria-label={`Scope actions for ${file.path}`} aria-expanded={headerMenu} onClick={() => setHeaderMenu(!headerMenu)}>⋯</Button>{headerMenu && <div aria-label={`Scope actions for file ${file.path}`}>{(['include', 'exclude'] as const).flatMap((action) => (['review', 'repository'] as const).map((lifetime) => <Button key={action + lifetime} disabled={busy} onClick={() => { setHeaderMenu(false); void setScope({ kind: 'file', expression: file.path, action, lifetime }); }}>{action === 'include' ? 'Include' : 'Exclude'} {lifetime === 'review' ? 'this review' : 'always'}</Button>))}</div>}<label className="workspace-button"><input type="checkbox" checked={fileState?.reviewStatus === 'reviewed-current'} disabled={busy || fileState?.scope.ignored} onChange={(event) => toggleViewed(event.target.checked)} /> Viewed</label></div></div>
      <div className="border-b border-line px-3 py-2 text-xs text-muted">{insightsLoading ? 'Loading insights…' : `${fileFindings.length} insights`} · Select a marker to discuss privately.</div>
      {(headerThreads.length || headerFindings.length) > 0 && <button type="button" className="thread-marker marker-group" aria-label={`Open ${headerThreads.length + headerFindings.length} file conversations; ${headerThreads.reduce((n, thread) => n + thread.unreadCount, 0)} unread; last ${headerThreads.at(-1)?.lastParticipant ?? 'insight'}`} onClick={() => openMarker(headerThreads, headerFindings.map((item) => ({ id: item.finding.id })))} />}
      {(unplacedThreads.length || unplacedFindings.length) > 0 && <div aria-label="Outdated and unplaced conversations">Outdated / unplaced <button type="button" className="thread-marker marker-group" aria-label={`Open ${unplacedThreads.length + unplacedFindings.length} outdated or unplaced conversations; ${unplacedThreads.reduce((n, thread) => n + thread.unreadCount, 0)} unread; last ${unplacedThreads.at(-1)?.lastParticipant ?? 'insight'}`} onClick={() => openMarker(unplacedThreads, unplacedFindings.map((item) => ({ id: item.finding.id })))} /></div>}
      {selectedThread && (selectedThread.subject === 'file' || selectedThread.anchorState !== 'current' || selectedThread.line === null) && <div className="mobile-conversation">{panel(true)}</div>}
      {fileState?.scope.ignored && <p className="p-4 text-sm text-muted">Ignored by review scope. Read-only; this file does not count toward progress.</p>}
      {rangeCandidate?.fileId === file.id && rangeCandidate.digest === file.diffDigest && !fileState?.scope.ignored && <div className="border-b p-2"><Button size="sm" variant="outline" onClick={() => openLineDraft(rangeCandidate)}>Draft selected lines</Button></div>}
      {otherDrafts.length > 0 && <nav className="flex flex-wrap gap-2 p-2" aria-label="Other unsent line drafts">{otherDrafts.map(([key, entry]) => <Button key={key} size="sm" variant="outline" onClick={() => openLineDraft(entry.anchor)}>Resume draft on {entry.anchor.side === 'LEFT' ? 'old' : 'new'} line {entry.anchor.line}</Button>)}</nav>}
      <DiffView diff={file} readOnly={!!fileState?.scope.ignored} threads={threads} findings={callouts.inline.filter((entry) => unthreaded(entry.item)).map((entry) => ({ id: entry.item.finding.id, side: entry.side, line: entry.startLine }))} selectedThreadId={selectedThreadId} selectedFindingId={selectedFindingId} onSelectLine={selectLine} onOpenMarker={(entries, items) => openMarker(entries, items)} onExplain={explainChange} renderAfterLine={(line) => <>{selectedThread && selectedThread.anchorState === 'current' && selectedThread.side !== null && (selectedThread.startLine ?? selectedThread.line) === (selectedThread.side === 'LEFT' ? line.oldLine : line.newLine) && <div className="mobile-conversation">{panel(true)}</div>}{validAnchor && activeDraftKey && anchor.line === (anchor.side === 'LEFT' ? line.oldLine : line.newLine) && <div className="border-y border-line bg-canvas p-3"><label htmlFor="draft">Draft on {anchor.side === 'LEFT' ? 'old' : 'new'} {anchor.startLine ? `lines ${anchor.startLine}–${anchor.line}` : `line ${anchor.line}`}</label><textarea id="draft" rows={3} className="workspace-input mt-2 w-full" placeholder="Write a review comment" value={draft} onChange={(event) => editLineDraft(activeDraftKey, event.target.value)} onKeyDown={(event) => { if (event.key === 'Enter' && (event.metaKey || event.ctrlKey) && !event.nativeEvent.isComposing && !event.repeat) { event.preventDefault(); saveDraft(); } }} /><div className="mt-2 flex gap-2"><Button disabled={busy || !draft.trim()} onClick={saveDraft}>Save draft</Button><Button onClick={() => discardLineDraft(activeDraftKey)}>Cancel</Button></div></div>}</>} />
      {staleDrafts.map(([key, entry]) => <div key={key} className="space-y-2 border-t p-3"><p role="alert" className="text-warning">Unsent draft for {entry.path}:{entry.anchor.line} is anchored to an earlier version. It will not be reanchored.</p><pre className="whitespace-pre-wrap wrap-anywhere">{entry.text}</pre><Button size="sm" variant="outline" onClick={() => discardLineDraft(key)}>Discard unsent draft</Button></div>)}
      <p className="workspace-diff-footer">Drafts and review progress are saved locally.</p></section>{selectedThread && <aside className="workspace-context desktop-conversation" style={{ marginTop: conversationTop }} aria-label="Selected conversation">{panel()}</aside>}</div> : <p className="workspace-panel p-4">No changed files to review.</p>}</section>
      <section id="workspace-conversations" hidden={activeView !== 'conversations'} aria-label="Conversations"><Conversations session={session} onOpenThread={openConversation} onReview={() => openReview()} /></section>
      <section id="workspace-feedback" hidden={activeView !== 'feedback'} aria-label="GitHub feedback"><Feedback threads={feedbackThreads} records={feedbackRecords} threadPage={feedbackThreadPage} recordPage={feedbackRecordPage} loading={feedbackLoading} errors={feedbackErrors} ignoredPaths={ignoredPaths} availablePaths={new Set(session.files.map((entry) => entry.path))} headSha={session.pr.headSha} onOpenFile={(path) => { const found = session.files.find((entry) => entry.path === path); if (found) openReview(found.id); }} /></section>
      <section id="workspace-finish" hidden={activeView !== 'finish'} aria-label="Finish review"><div className="workspace-panel p-4" aria-label="Draft publication selection"><h3>Choose prepared review comments to include</h3><p>Selection applies to every conversation, including those outside the current file. Unselected comments stay local.</p>{allThreads.filter((thread) => thread.reviewComment).map((thread) => <div key={thread.id} className="mt-3 border-t border-line pt-2"><p>{thread.path}{thread.line === null ? '' : `:${thread.line}`}</p>{draftSelection(thread)}</div>)}</div><FinishReview session={session} onSession={(view) => controller.current?.receive(view)} /></section>
    </main></div>{explainTarget && <ExplainPanel key={JSON.stringify(explainTarget.selection)} target={explainTarget} stale={!selectionCurrent(explainTarget.selection, session, file?.id)} onClose={() => { setExplainTarget(null); explainTrigger.current?.focus({ preventScroll: true }); }} />}
  </div>;
}
