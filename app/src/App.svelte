<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    ApiError, createInsightThread, createScopeRule, createThread, fetchFeedbackRecords, fetchFeedbackThreads, fetchFindings, fetchQueue, fetchSession, fetchExtensionPreferences, patchThread, previewReview, putExtensionPreference, setReviewed, submitReview, watchSession,
    type ConversationThread, type FeedbackThread, type FindingItem, type NativeFeedbackRecord, type QueueView, type SessionView,
  } from './api';
  import { placeCallouts } from './callouts';
  import ConversationPanel from './workspace/ConversationPanel.svelte';
  import type { ConversationDraft } from './workspace/ConversationPanel.svelte';
  import SubmitReview from './workspace/SubmitReview.svelte';
  import ReviewPrep from './workspace/ReviewPrep.svelte';
  import Conversations from './workspace/Conversations.svelte';
  import Feedback from './workspace/Feedback.svelte';
  import type { Page } from './generated/Page';
  import { unresolvedByPath } from './feedback';
  import FileNavigator from './workspace/FileNavigator.svelte';
  import type { DiffLine } from './generated/DiffLine';
  import type { Side } from './generated/Side';
  import type { FileStatus } from './generated/FileStatus';
  import { nextUnreviewedFile } from './fileTree';
  import { orderThreads, threadsForLine } from './threadMarkers';

  let session = $state<SessionView | null>(null);
  let error = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let busy = $state(false);
  let selectedId = $state<string | null>(null);
  let anchor = $state<{ fileId: string; digest: string; side: Side; line: number; startLine?: number } | null>(null);
  let draft = $state('');
  let selectedThreadId = $state<string | null>(null);
  let conversationDrafts = $state<Record<string, ConversationDraft>>({});
  let conversationTop = $state(0);
  function alignConversation() {
    if (!selectedThreadId || activeView !== 'files' || window.matchMedia?.('(max-width: 1119px)').matches) return;
    const thread = allThreads.find((entry) => entry.id === selectedThreadId);
    const rail = document.querySelector<HTMLElement>('.desktop-conversation');
    const panel = rail?.querySelector<HTMLElement>('.review-thread');
    const diff = document.getElementById('file-review');
    const anchor = thread?.subject === 'file' || thread?.anchorState !== 'current' || thread?.line === null
      ? diff?.querySelector('.workspace-diff-header')
      : [...(diff?.querySelectorAll<HTMLElement>('.diff-row') ?? [])].find((row) => {
          const code = row.querySelector<HTMLElement>('[data-code-line]');
          return code?.dataset.side === thread?.side && Number(code?.dataset.codeLine) === (thread.startLine ?? thread.line);
        });
    if (panel && anchor) {
      const delta = anchor.getBoundingClientRect().top - panel.getBoundingClientRect().top;
      if (Math.abs(delta) > 1) conversationTop = Math.max(0, conversationTop + delta);
    }
  }
  $effect(() => {
    selectedThreadId;
    session?.sequence;
    if (selectedThreadId) void tick().then(() => requestAnimationFrame(alignConversation));
  });
  const views = [
    { id: 'prep', label: 'Review prep' },
    { id: 'files', label: 'Files changed' },
    { id: 'conversations', label: 'Conversations' },
    { id: 'feedback', label: 'GitHub feedback' },
  ] as const;
  type WorkspaceView = (typeof views)[number]['id'];
  // Start with review prep so focus areas are visible before opening a diff.
  let activeView = $state<WorkspaceView>('prep');
  const fileStatusLabels: Record<FileStatus, string> = {
    ignored: 'Ignored',
    'reviewed-current': 'Viewed',
    'changed-since-reviewed': 'Changed since review',
    unreviewed: 'Unreviewed',
  };

  let scopeError = $state<string | null>(null);
  let typePreferences = $state<Record<string, 'include' | 'exclude'>>({});
  let preferenceRequest = 0;
  function loadPreferences() {
    const request = ++preferenceRequest;
    void fetchExtensionPreferences().then(({ rules }) => {
      if (request !== preferenceRequest) return;
      const next = Object.fromEntries(rules.filter((rule) => rule.enabled).map((rule) => [rule.category, rule.action])) as typeof typePreferences;
      if (JSON.stringify(next) !== JSON.stringify(typePreferences)) typePreferences = next;
    }).catch((failure) => { if (request === preferenceRequest) scopeError = failure instanceof Error ? failure.message : String(failure); });
  }
  let lastScope = $state<ScopeTarget | null>(null);
  let headerMenu = $state(false);
  // Review insights (#38) load beside the review, never before it: the diff
  // renders from the session view alone and insights fill in as they arrive.
  let queue = $state<QueueView | null>(null);
  let findingItems = $state<FindingItem[] | null>(null);
  let findingsSnapshotId = $state<string | null>(null);
  let selectedFindingId = $state<string | null>(null);
  let insightsError = $state<string | null>(null);
  let insightsLoading = $state(false);
  let insightsRequest = 0;
  let feedbackThreads = $state<FeedbackThread[]>([]);
  let feedbackRecords = $state<NativeFeedbackRecord[]>([]);
  let feedbackThreadPage = $state<Page<FeedbackThread> | null>(null);
  let feedbackRecordPage = $state<Page<NativeFeedbackRecord> | null>(null);
  let feedbackErrors = $state<string[]>([]);
  let feedbackLoading = $state(false);
  let feedbackRequest = 0;

  function loadFeedback() {
    const request = ++feedbackRequest;
    feedbackLoading = true;
    feedbackErrors = [];
    void Promise.allSettled([fetchFeedbackThreads(), fetchFeedbackRecords()]).then(([threadsResult, recordsResult]) => {
      if (request !== feedbackRequest) return;
      feedbackLoading = false;
      if (threadsResult.status === 'fulfilled') {
        feedbackThreads = threadsResult.value.items;
        feedbackThreadPage = threadsResult.value.last;
      } else {
        feedbackThreads = [];
        feedbackThreadPage = null;
        feedbackErrors.push(threadsResult.reason instanceof Error ? threadsResult.reason.message : String(threadsResult.reason));
      }
      if (recordsResult.status === 'fulfilled') {
        feedbackRecords = recordsResult.value.items;
        feedbackRecordPage = recordsResult.value.last;
      } else {
        feedbackRecords = [];
        feedbackRecordPage = null;
        feedbackErrors.push(recordsResult.reason instanceof Error ? recordsResult.reason.message : String(recordsResult.reason));
      }
    });
  }

  /** Fire-and-forget; read-only. Latest request wins. */
  function loadInsights() {
    const request = ++insightsRequest;
    insightsLoading = true;
    void Promise.allSettled([fetchQueue(), fetchFindings()]).then(([q, f]) => {
      if (request !== insightsRequest) return;
      insightsLoading = false;
      if (q.status === 'fulfilled') queue = q.value;
      if (f.status === 'fulfilled') {
        findingItems = f.value.items;
        findingsSnapshotId = f.value.last.snapshotId;
      }
      const failed = [q, f].find((r) => r.status === 'rejected') as PromiseRejectedResult | undefined;
      insightsError = failed ? (failed.reason instanceof Error ? failed.reason.message : String(failed.reason)) : null;
    });
  }

  onMount(() => {
    return watchSession(
      (view) => { session = view; error = null; loadInsights(); loadFeedback(); loadPreferences(); },
      (failure) => { error = String(failure); },
    );
  });

  const file = $derived(session?.files.find((item) => item.id === selectedId) ?? session?.files[0]);
  const statesById = $derived(new Map(session?.fileStates.map((entry) => [entry.fileId, entry])));
  const fileState = $derived(file && statesById.get(file.id));
  const conversationCount = $derived(session ? session.threads.length + session.reviews.length + session.issueComments.length : 0);
  const ignoredFeedbackPaths = $derived(new Set(session?.fileStates.filter((state) => state.scope.ignored)
    .map((state) => session?.files.find((item) => item.id === state.fileId)?.path)
    .filter((path): path is string => !!path) ?? []));
  const allThreads = $derived((session?.threads ?? []) as ConversationThread[]);
  const threads = $derived(allThreads.filter((thread) => thread.fileId === file?.id || thread.path === file?.path));
  const sortedThreads = $derived(orderThreads(threads));
  const totals = $derived(
    session?.files.reduce(
      (acc, item) => ({ add: acc.add + item.additions, del: acc.del + item.deletions }),
      { add: 0, del: 0 },
    ) ?? { add: 0, del: 0 },
  );

  function selectFile(id: string) {
    selectedId = id;
    headerMenu = false;
    selectedThreadId = null;
    selectedFindingId = null;
    anchor = null;
    draft = '';
    actionError = null;
  }

  function openReview(id?: string) {
    const movingToDiff = activeView !== 'files' || window.matchMedia?.('(max-width: 760px)').matches;
    if (id && id !== file?.id) selectFile(id);
    activeView = 'files';
    if (movingToDiff) void tick().then(() => document.getElementById('file-review-heading')?.focus());
  }

  function openFeedbackFile(path: string) {
    const target = session?.files.find((item) => item.path === path);
    if (!target) return;
    openReview(target.id);
  }

  function openPrep() {
    activeView = 'prep';
    void tick().then(() => document.getElementById('review-prep-heading')?.focus());
  }

  function startReview() {
    const included = fileState?.scope.ignored
      ? session?.files.find((entry) => !statesById.get(entry.id)?.scope.ignored)
      : undefined;
    openReview(included?.id);
  }

  function openConversation(fileId: string, threadId: string) {
    if (fileId !== file?.id) selectFile(fileId);
    activeView = 'files';
    void showThread(threadId);
  }

  const fileFindings = $derived((findingItems ?? []).filter((item) =>
    item.finding.anchor.fileId === file?.id || item.finding.anchor.currentPath === file?.path));
  const callouts = $derived(placeCallouts(fileFindings, file ? [file] : [],
    findingsSnapshotId === session?.analysis.snapshotId ? session.analysis.snapshotId : '', 'all', session?.threads ?? []));
  const selectedThread = $derived(allThreads.find((thread) => thread.id === selectedThreadId));
  const headerThreads = $derived(threads.filter((thread) => thread.subject === 'file' && thread.anchorState === 'current'));
  const unplacedThreads = $derived(threads.filter((thread) => thread.anchorState !== 'current' || (thread.subject !== 'file' && thread.line === null)));
  const headerFindings = $derived(callouts.file.filter((entry) => !allThreads.some((thread) => thread.findingId === entry.item.finding.id)));
  const unplacedFindings = $derived(callouts.unplaced.filter((entry) => !allThreads.some((thread) => thread.findingId === entry.item.finding.id)));
  function openMarker(groupThreads: ConversationThread[], groupFindings: FindingItem[]) {
    const index = groupThreads.findIndex((thread) => thread.id === selectedThreadId);
    if (index >= 0 && index + 1 < groupThreads.length) { void showThread(groupThreads[index + 1].id); return; }
    if (index < 0 && groupThreads.length && !groupFindings.some((item) => item.finding.id === selectedFindingId)) {
      void showThread(groupThreads[0].id); return;
    }
    const findingIndex = groupFindings.findIndex((item) => item.finding.id === selectedFindingId);
    const finding = groupFindings[findingIndex >= 0 ? (findingIndex + 1) % groupFindings.length : 0];
    if (finding) void showFinding(finding.finding.id);
    else if (groupThreads.length) void showThread(groupThreads[0].id);
  }

  async function showFinding(id: string) {
    const finding = fileFindings.find((item) => item.finding.id === id);
    if (!finding || !file) return;
    selectedFindingId = id;
    const existing = allThreads.find((thread) => thread.findingId === id);
    if (existing) { await showThread(existing.id); return; }
    const anchor = finding.finding.anchor;
    const side = anchor.side ?? 'RIGHT';
    const line = anchor.line ?? undefined;
    await act(() => createInsightThread(file.id, file.diffDigest, id, side, line, line === undefined ? undefined : anchor.startLine ?? undefined));
    const created = allThreads.find((thread) => thread.findingId === id);
    if (created) await showThread(created.id);
  }

  function openPrepFinding(fileId: string, findingId?: string) {
    openReview(fileId);
    if (findingId) void tick().then(() => showFinding(findingId));
  }

  function openPrepThread(fileId: string, threadId: string) {
    openReview(fileId);
    void showThread(threadId);
  }

  type ScopeTarget = { kind: 'file' | 'directory' | 'glob'; expression: string; action: 'include' | 'exclude'; lifetime: 'review' | 'repository' };
  async function setScope(target: ScopeTarget) {
    if (busy) return;
    lastScope = target;
    scopeError = null;
    busy = true;
    try {
      await createScopeRule(target.action, target.kind, target.expression, target.lifetime);
      session = await fetchSession();
      loadInsights();
    } catch (failure) {
      scopeError = failure instanceof Error ? failure.message : String(failure);
    } finally {
      busy = false;
    }
  }

  async function setTypePreference(extension: string, mode: 'default' | 'include' | 'exclude') {
    if (busy) return;
    scopeError = null;
    busy = true;
    try {
      await putExtensionPreference(extension, mode);
      session = await fetchSession();
      loadPreferences();
      loadInsights();
    } catch (failure) {
      scopeError = failure instanceof Error ? failure.message : String(failure);
      const selector = [...document.querySelectorAll<HTMLSelectElement>('select[aria-label^="Repository preference for "]')]
        .find((element) => element.getAttribute('aria-label') === `Repository preference for ${extension}`);
      if (selector) selector.value = typePreferences[extension] ?? 'default';
      loadPreferences();
    } finally { busy = false; }
  }

  function toggleViewed(checked: boolean) {
    if (!file || fileState?.scope.ignored) return;
    const { id, diffDigest } = file;
    void act(() => setReviewed(id, diffDigest, checked), () => {
      if (!checked || !session) return;
      const next = nextUnreviewedFile(session.files, session.fileStates, id);
      if (next) {
        selectFile(next.id);
        void tick().then(() => document.getElementById('file-review')?.scrollIntoView({ block: 'start' }));
      }
    });
  }

  function lineSide(line: DiffLine): Side | null {
    if (line.kind === 'meta') return null;
    return line.kind === 'deletion' ? 'LEFT' : 'RIGHT';
  }

  function selectCode() {
    if (!file || !file.patchComplete || fileState?.scope.ignored) return;
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || !selection.toString().trim() || selection.rangeCount === 0) return;
    const range = selection.getRangeAt(0);
    const code = document.querySelector('#file-review .workspace-code');
    if (!code || !code.contains(range.commonAncestorContainer)) return;
    if ([range.startContainer, range.endContainer].some((node) =>
      (node instanceof Element ? node : node.parentElement)?.closest('[data-callout-scope], .callout-band'))) return;
    const lines = [...code.querySelectorAll<HTMLElement>('[data-code-line]')]
      .filter((element) => range.intersectsNode(element));
    if (!lines.length || lines.some((element) => !element.dataset.side || element.dataset.side !== lines[0].dataset.side)) return;
    const numbers = lines.map((element) => Number(element.dataset.codeLine));
    if (numbers.some((number) => !Number.isInteger(number)) || numbers.at(-1)! - numbers[0] !== numbers.length - 1) return;
    anchor = { fileId: file.id, digest: file.diffDigest, side: lines[0].dataset.side as Side,
      line: numbers.at(-1)!, ...(numbers.length > 1 ? { startLine: numbers[0] } : {}) };
    draft = '';
    actionError = null;
    selection.removeAllRanges();
  }

  function selectLine(line: DiffLine) {
    if (!file || !file.patchComplete || fileState?.scope.ignored) return;
    const side = lineSide(line);
    const number = side === 'LEFT' ? line.oldLine : line.newLine;
    if (!side || number === null) return;
    anchor = { fileId: file.id, digest: file.diffDigest, side, line: number };
    draft = '';
    actionError = null;
    void tick().then(() => document.getElementById('draft')?.focus());
  }

  async function showThread(threadId: string) {
    selectedThreadId = threadId;
    await tick();
    const mobile = window.matchMedia?.('(max-width: 1119px)').matches;
    const card = document.getElementById(`${mobile ? 'thread-mobile-' : 'thread-'}${threadId}`);
    card?.focus({ preventScroll: true });
    alignConversation();
    if (mobile) card?.scrollIntoView({ block: 'nearest' });
    const thread = allThreads.find((item) => item.id === threadId);
    const last = thread?.comments.at(-1);
    if (thread?.source === 'local' && last && thread.unreadCount) void act(() => patchThread(threadId, { readThrough: last.id }));
  }

  async function act(operation: () => Promise<void>, onSuccess?: () => void) {
    if (busy) return;
    busy = true;
    actionError = null;
    try {
      await operation();
      session = await fetchSession();
      loadInsights();
      onSuccess?.();
    } catch (failure) {
      if (failure instanceof ApiError && failure.status === 409) {
        anchor = null;
        try {
          session = await fetchSession();
          actionError = 'The diff changed. Review the latest file before trying again.';
        } catch (refreshFailure) {
          actionError = `The diff changed, but refresh failed: ${String(refreshFailure)}`;
        }
      } else {
        actionError = failure instanceof Error ? failure.message : String(failure);
      }
    } finally {
      busy = false;
    }
  }

  function saveDraft() {
    if (!file || !anchor || !draft.trim() || anchor.fileId !== file.id || anchor.digest !== file.diffDigest || fileState?.scope.ignored) return;
    const { fileId, digest, side, line, startLine } = anchor;
    const body = draft.trim();
    void act(() => createThread(fileId, digest, side, line, body, startLine), () => { anchor = null; draft = ''; });
  }


</script>

<svelte:window onmouseup={selectCode} onkeyup={selectCode} onresize={() => requestAnimationFrame(alignConversation)} onscroll={() => requestAnimationFrame(alignConversation)} />

{#snippet threadNavigation()}
  <nav class="mb-2 flex justify-between gap-2 text-xs" aria-label="Conversation navigation">
    <button type="button" class="workspace-button" disabled={sortedThreads.findIndex((thread) => thread.id === selectedThreadId) <= 0} onclick={() => void showThread(sortedThreads[sortedThreads.findIndex((thread) => thread.id === selectedThreadId) - 1].id)}>Previous</button>
    <button type="button" class="workspace-button" disabled={sortedThreads.findIndex((thread) => thread.id === selectedThreadId) >= sortedThreads.length - 1} onclick={() => void showThread(sortedThreads[sortedThreads.findIndex((thread) => thread.id === selectedThreadId) + 1].id)}>Next</button>
  </nav>
{/snippet}

<div class="workspace">
  {#if !session}
    <header class="workspace-toolbar"><p class="font-semibold">Aside</p></header>
    {#if error}<p class="p-6 text-danger" role="alert">{error}</p>
    {:else}<p class="p-6 text-muted" role="status">Loading review…</p>{/if}
  {:else}
    <header class="workspace-toolbar">
      <div class="min-w-0">
        <p class="workspace-crumb">{session.pr.owner} / {session.pr.repo} <span>/ pull requests /</span> #{session.pr.number}</p>
        <h1 class="workspace-title">{session.pr.title} <span>#{session.pr.number}</span></h1>
        <p class="mt-1 text-xs text-muted wrap-anywhere">
          {session.pr.author} wants to merge into <code class="workspace-ref">{session.pr.baseRef}</code>
          from <code class="workspace-ref">{session.pr.headRef}</code>
        </p>
      </div>
      <div class="flex min-w-0 max-w-full flex-wrap items-center gap-2">
        <span class="workspace-status" class:is-open={session.pr.state.toLowerCase() === 'open' && !session.pr.isDraft}>
          <span class="status-dot" aria-hidden="true"></span>{session.pr.isDraft ? 'Draft' : session.pr.state.toLowerCase()}
        </span>
        {#if session.demo}<span class="workspace-chip">Demo · local only</span>{/if}
        <SubmitReview threads={session.threads} demo={session.demo}
          onPreview={previewReview} onSubmit={async (input) => { const response = await submitReview(input); session = await fetchSession(); return response; }}
          onReconciled={async () => { session = await fetchSession(); }} />
      </div>
    </header>

    <nav class="workspace-tabs" aria-label="Review workspace">
      {#each views as view (view.id)}
        <button type="button" class="workspace-tab" aria-pressed={activeView === view.id}
          aria-controls={'workspace-' + view.id} onclick={() => { activeView = view.id; }}>
          {view.label}
          {#if view.id === 'files'}<span class="workspace-count">{session.files.length}</span>
          {:else if view.id === 'conversations'}<span class="workspace-count">{conversationCount}</span>{/if}
        </button>
      {/each}
    </nav>

    {#if error || actionError || session.warnings.length}
      <div class="workspace-notices">
        {#if error}<p class="text-danger" role="alert">Refresh failed: {error}</p>{/if}
        {#if actionError}<p class="text-danger" role="alert">{actionError}</p>{/if}
        {#each session.warnings as warning}<p class="text-warning">{warning}</p>{/each}
      </div>
    {/if}

    <div class="workspace-body">
      <aside class="workspace-sidebar" aria-label="Review file tree">
        <div class="sidebar-content">
          <div class="sidebar-heading"><h2>Changed files <span>{session.progress.inScope} / {session.files.length}</span></h2></div>
          <FileNavigator files={session.files} states={session.fileStates} {queue} rules={session.rules}
            selectedId={file?.id} {typePreferences} pending={busy} error={scopeError} onRetry={() => { if (lastScope) void setScope(lastScope); }}
            onSelect={openReview} onScope={(target) => void setScope(target)} onTypePreference={setTypePreference} />
        </div>
      </aside>

      <main class="workspace-main">
        <section id="workspace-prep" hidden={activeView !== 'prep'} aria-label="Review prep">
          <ReviewPrep {session} {queue} findings={findingsSnapshotId === session.analysis.snapshotId ? findingItems : null}
            threads={session.threads} {insightsError} onOpenFile={openPrepFinding} onOpenThread={openPrepThread} onStart={startReview} />
        </section>
        <section id="workspace-files" hidden={activeView !== 'files'} aria-label="Files changed">
          <div class="workspace-view-heading">
            <div>
              <p class="workspace-eyebrow">Files changed · Review</p>
              <h2 id="file-review-heading" tabindex="-1">{fileState?.reviewStatus === 'reviewed-current' ? 'This version is already reviewed.' : 'Pick up where you left off.'}</h2>
              <p class="workspace-subtitle">
                {session.progress.reviewed} of {session.progress.inScope} in-scope files reviewed
                · {session.progress.changedSinceReviewed} changed since review
                · {session.progress.ignored} ignored
                · <span class="text-added">+{totals.add}</span> <span class="text-deleted">−{totals.del}</span>
              </p>
            </div>
          </div>
          {#if file}
            <div class="workspace-review-grid" class:conversation-open={!!selectedThread}>
              <section id="file-review" class="workspace-diff" aria-label="File review">
                <div class="workspace-diff-header">
                  <div class="flex min-w-0 items-center gap-2">
                    <div class="min-w-0">
                      <div class="flex items-center gap-2"><h3 class="wrap-anywhere font-mono text-sm font-semibold">{file.path}</h3>
                        {#if headerThreads.length || headerFindings.length}
                          <button type="button" class="thread-marker marker-group" class:has-unread={headerThreads.some((thread) => thread.unreadCount > 0)}
                            style={`--participant:${headerThreads.at(-1)?.lastParticipant === 'agent' ? '#7c3aed' : '#2563eb'}`}
                            aria-label={`Open ${headerThreads.length + headerFindings.length} file ${headerThreads.length + headerFindings.length === 1 ? 'conversation' : 'conversations'}; ${headerThreads.reduce((count, thread) => count + (thread.unreadCount ?? 0), 0)} unread; last ${headerThreads.at(-1)?.lastParticipant ?? 'insight'}`}
                            aria-pressed={headerThreads.some((thread) => thread.id === selectedThreadId) || headerFindings.some((entry) => entry.item.finding.id === selectedFindingId)}
                            onclick={() => openMarker(headerThreads, headerFindings.map((entry) => entry.item))}></button>
                          {#if headerThreads.length + headerFindings.length > 1}<span class="text-xs text-muted">{headerThreads.length + headerFindings.length}</span>{/if}
                        {/if}
                      </div>
                      <p class="text-xs text-muted">
                        {file.changeKind} · {fileStatusLabels[fileState?.status ?? 'unreviewed']}
                        {#if file.previousPath} · from {file.previousPath}{/if}
                        {#if !file.patchComplete} · incomplete patch{/if}
                      </p>
                    </div>
                  </div>
                  <div class="workspace-diff-actions">
                    <span class="font-mono text-xs"><span class="text-added">+{file.additions}</span> <span class="text-deleted">−{file.deletions}</span></span>
                    <div class="relative">
                      <button type="button" class="workspace-button" aria-label={`Scope actions for ${file.path}`} aria-expanded={headerMenu} onclick={() => { headerMenu = !headerMenu; }}>⋯</button>
                      {#if headerMenu}
                        <div class="absolute right-0 z-20 grid min-w-44 rounded border border-line bg-canvas p-1 shadow-lg" aria-label={`Scope actions for file ${file.path}`}>
                          <p class="px-2 text-xs text-muted">{fileState?.scope.ignored ? 'Excluded' : 'Included'}{#if fileState?.scope.effectiveRuleId && session.rules.find((rule) => rule.id === fileState.scope.effectiveRuleId)} by {session.rules.find((rule) => rule.id === fileState.scope.effectiveRuleId)?.kind} rule{/if}</p>
                          {#each ['include', 'exclude'] as action}
                            {#each ['review', 'repository'] as lifetime}
                              <button type="button" class="workspace-button text-left" disabled={busy} onclick={() => { headerMenu = false; void setScope({ kind: 'file', expression: file.path, action: action as 'include' | 'exclude', lifetime: lifetime as 'review' | 'repository' }); }}>
                                {action === 'include' ? 'Include' : 'Exclude'} {lifetime === 'review' ? 'this review' : 'always'}
                              </button>
                            {/each}
                          {/each}
                        </div>
                      {/if}
                    </div>
                    <label class="workspace-button" class:opacity-50={busy || fileState?.scope.ignored}>
                      <input type="checkbox" checked={fileState?.reviewStatus === 'reviewed-current'} disabled={busy || fileState?.scope.ignored}
                        onchange={(event) => { const next = event.currentTarget.checked; event.currentTarget.checked = fileState?.reviewStatus === 'reviewed-current'; toggleViewed(next); }}
                        class="accent-accent" /> Viewed
                    </label>
                  </div>
                </div>
                  <div class="border-b border-line px-3 py-2 text-xs text-muted">{insightsLoading ? 'Loading insights…' : `${fileFindings.length} insights`} · Select a marker to discuss privately.</div>
                  {#if unplacedThreads.length || unplacedFindings.length}
                    <div class="flex items-center gap-2 border-b border-line px-3 py-1 text-xs text-muted" aria-label="Outdated and unplaced conversations">
                      <span>Outdated / unplaced</span>
                      <button type="button" class="thread-marker marker-group" class:has-unread={unplacedThreads.some((thread) => thread.unreadCount > 0)}
                        aria-label={`Open ${unplacedThreads.length + unplacedFindings.length} outdated or unplaced conversations; ${unplacedThreads.reduce((count, thread) => count + (thread.unreadCount ?? 0), 0)} unread; last ${unplacedThreads.at(-1)?.lastParticipant ?? 'insight'}`}
                        aria-pressed={unplacedThreads.some((thread) => thread.id === selectedThreadId) || unplacedFindings.some((entry) => entry.item.finding.id === selectedFindingId)}
                        onclick={() => openMarker(unplacedThreads, unplacedFindings.map((entry) => entry.item))}></button>
                      {#if unplacedThreads.length + unplacedFindings.length > 1}<span>{unplacedThreads.length + unplacedFindings.length}</span>{/if}
                    </div>
                  {/if}
                  {#if selectedThread && (selectedThread.subject === 'file' || selectedThread.anchorState !== 'current' || selectedThread.line === null)}
                    <div class="mobile-conversation p-2">{@render threadNavigation()}<ConversationPanel thread={selectedThread} drafts={conversationDrafts} mobile agentConnected={session.agentPresence?.connected ?? false} finding={fileFindings.find((item) => item.finding.id === selectedThread.findingId)}
                      onClose={() => { selectedThreadId = null; }} onError={(message) => { actionError = message; }} onRefresh={async () => { session = await fetchSession(); }} /></div>
                  {/if}
                  {#if fileState?.scope.ignored}
                    <p class="border-b border-line p-4 text-sm text-muted">Ignored by review scope. Read-only; this file does not count toward progress.</p>
                  {/if}
                  {#if file.isBinary || file.hunks.length === 0}
                    <p class="p-4 text-sm text-muted">{file.isBinary ? 'Binary file: no text diff.' : 'No text hunks available for this file.'}</p>
                  {:else}
                    <div class="workspace-code">
                      {#each file.hunks as hunk (hunk.id)}
                        <div class="min-w-max border-b border-line bg-accent-soft px-3 py-1 text-muted">@@ -{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},{hunk.newCount} @@ {hunk.header}</div>
                        {#each hunk.lines as line (line.id)}
                          {@const lineThreads = threadsForLine(line, threads)}
                          {@const lineFindings = callouts.inline.filter((entry) => entry.startLine === (entry.side === 'LEFT' ? line.oldLine : line.newLine) && !allThreads.some((thread) => thread.findingId === entry.item.finding.id))}
                          {@const rangeHighlighted = threads.some((thread) => thread.id === selectedThreadId && thread.anchorState === 'current' && thread.side === lineSide(line) && (lineSide(line) === 'LEFT' ? line.oldLine : line.newLine)! >= (thread.startLine ?? thread.line ?? Infinity) && (lineSide(line) === 'LEFT' ? line.oldLine : line.newLine)! <= (thread.line ?? -1))}
                          <div class="diff-row flex min-w-max whitespace-pre" data-thread-anchor={lineThreads.map((thread) => thread.id).join(' ')} class:bg-added-soft={line.kind === 'addition'} class:bg-deleted-soft={line.kind === 'deletion'} class:bg-accent-soft={rangeHighlighted}>
                            <span class="w-12 shrink-0 select-none border-r border-line px-2 text-right text-muted">{line.oldLine ?? ''}</span>
                            <span class="w-12 shrink-0 select-none border-r border-line px-2 text-right text-muted">{line.newLine ?? ''}</span>
                            <span class="flex w-9 shrink-0 items-center justify-center gap-1">
                              {#each ['LEFT', 'RIGHT'] as markerSide}
                                {@const sideThreads = lineThreads.filter((thread) => thread.side === markerSide)}
                                {@const sideFindings = lineFindings.filter((entry) => entry.side === markerSide)}
                                {#if line.kind !== 'meta' && (sideThreads.length || sideFindings.length)}
                                  <button type="button" onclick={() => openMarker(sideThreads, sideFindings.map((entry) => entry.item))}
                                    aria-label={`Open ${sideThreads.length + sideFindings.length} ${sideThreads.length + sideFindings.length === 1 ? 'conversation' : 'conversations'} on ${markerSide === 'LEFT' ? 'old' : 'new'} line ${markerSide === 'LEFT' ? line.oldLine : line.newLine}; ${sideThreads.reduce((count, thread) => count + (thread.unreadCount ?? 0), 0)} unread; last ${sideThreads.at(-1)?.lastParticipant ?? 'insight'}`}
                                    aria-pressed={sideThreads.some((thread) => thread.id === selectedThreadId) || sideFindings.some((entry) => entry.item.finding.id === selectedFindingId)}
                                    title={`${sideThreads.length + sideFindings.length} conversations · ${sideThreads.reduce((count, thread) => count + (thread.unreadCount ?? 0), 0)} unread · last ${sideThreads.at(-1)?.lastParticipant ?? 'insight'}`}
                                    class="thread-marker marker-group" class:has-unread={sideThreads.some((thread) => thread.unreadCount > 0)} style={`--participant:${sideThreads.at(-1)?.lastParticipant === 'agent' ? '#7c3aed' : '#2563eb'}`}></button>
                                  {#if sideThreads.length + sideFindings.length > 1}<span class="text-[10px]">{sideThreads.length + sideFindings.length}</span>{/if}
                                {/if}
                              {/each}
                            </span>
                            {#if lineSide(line) && file.patchComplete && !fileState?.scope.ignored}
                              <button type="button" onclick={() => selectLine(line)} title="Draft a comment on this line" class="line-draft w-7 shrink-0 text-center text-accent hover:bg-accent-soft focus-visible:outline-2 focus-visible:outline-accent">+</button>
                            {:else}
                              <span class="w-7 shrink-0"></span>
                            {/if}
                            <span class="px-2" data-code-line={lineSide(line) ? (lineSide(line) === 'LEFT' ? line.oldLine : line.newLine) : undefined} data-side={lineSide(line) ?? undefined}>{line.kind === 'addition' ? '+' : line.kind === 'deletion' ? '-' : ' '}{line.text}</span>
                          </div>
                          {#if selectedThread && lineThreads.some((thread) => thread.id === selectedThreadId)}
                            <div class="mobile-conversation min-w-0 whitespace-normal p-2">{@render threadNavigation()}<ConversationPanel thread={selectedThread} drafts={conversationDrafts} mobile agentConnected={session.agentPresence?.connected ?? false} finding={fileFindings.find((item) => item.finding.id === selectedThread.findingId)}
                              onClose={() => { selectedThreadId = null; }} onError={(message) => { actionError = message; }} onRefresh={async () => { session = await fetchSession(); }} /></div>
                          {/if}
                          {#if !fileState?.scope.ignored && anchor?.fileId === file.id && anchor.digest === file.diffDigest && anchor.side === lineSide(line) && anchor.line === (anchor.side === 'LEFT' ? line.oldLine : line.newLine)}
                            <div class="min-w-0 border-y border-line bg-canvas p-3 font-sans whitespace-normal">
                              <label for="draft" class="block text-sm font-semibold">Draft on {anchor.side === 'LEFT' ? 'old' : 'new'} {anchor.startLine ? `lines ${anchor.startLine}–${anchor.line}` : `line ${anchor.line}`}</label>
                              <textarea id="draft" bind:value={draft} rows="3" class="workspace-input mt-2 w-full" placeholder="Write a review comment" onkeydown={(event) => {
                                if (event.key === 'Enter' && (event.metaKey || event.ctrlKey) && !event.isComposing && !event.repeat) {
                                  event.preventDefault();
                                  saveDraft();
                                }
                              }}></textarea>
                              <div class="mt-2 flex gap-2">
                                <button type="button" disabled={busy || !draft.trim()} onclick={saveDraft} class="workspace-button primary-button">Save draft</button>
                                <button type="button" onclick={() => { anchor = null; draft = ''; }} class="workspace-button">Cancel</button>
                              </div>
                            </div>
                          {/if}
                        {/each}
                      {/each}
                    </div>
                  {/if}
                <p class="workspace-diff-footer">Drafts and review progress are saved locally.</p>
              </section>

              {#if selectedThread}
                <aside class="workspace-context desktop-conversation" style={`margin-top:${conversationTop}px`} aria-label="Selected conversation">
                  {@render threadNavigation()}
                  <ConversationPanel thread={selectedThread} drafts={conversationDrafts} agentConnected={session.agentPresence?.connected ?? false} finding={fileFindings.find((item) => item.finding.id === selectedThread.findingId)}
                    onClose={() => { selectedThreadId = null; selectedFindingId = null; }} onError={(message) => { actionError = message; }} onRefresh={async () => { session = await fetchSession(); }} />
                </aside>
              {/if}
            </div>
          {:else}
            <p class="workspace-panel p-4 text-sm text-muted">No changed files to review.</p>
          {/if}
        </section>

        <section id="workspace-feedback" hidden={activeView !== 'feedback'} aria-label="GitHub feedback">
          <Feedback threads={feedbackThreads} records={feedbackRecords} threadPage={feedbackThreadPage} recordPage={feedbackRecordPage}
            loading={feedbackLoading} errors={feedbackErrors}
            ignoredPaths={ignoredFeedbackPaths} availablePaths={new Set(session.files.map((item) => item.path))} headSha={session.pr.headSha}
            onOpenFile={openFeedbackFile} />
        </section>
        <section id="workspace-conversations" hidden={activeView !== 'conversations'} aria-label="Conversations">
          <Conversations {session} onOpenThread={openConversation} onReview={() => openReview()} />
        </section>
      </main>
    </div>
  {/if}
</div>
