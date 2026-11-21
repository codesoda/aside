<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    ApiError, createFileRule, createThread, fetchFindings, fetchQueue, fetchSession, replyToThread, setReviewed, watchSession,
    type FindingItem, type QueueView, type SessionView,
  } from './api';
  import { fileIndicators, type QueueFilter } from './insights';
  import ReviewPrep from './workspace/ReviewPrep.svelte';
  import InsightsPanel from './insights/InsightsPanel.svelte';
  import Conversations from './workspace/Conversations.svelte';
  import ReviewQueue from './insights/ReviewQueue.svelte';
  import type { DiffLine } from './generated/DiffLine';
  import type { Side } from './generated/Side';
  import type { FileStatus } from './generated/FileStatus';
  import type { ChangedFile } from './generated/ChangedFile';
  import { buildFileTree, nextUnreviewedFile, type FileTreeNode } from './fileTree';
  import { orderThreads, threadsForLine } from './threadMarkers';

  let session = $state<SessionView | null>(null);
  let error = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let busy = $state(false);
  let selectedId = $state<string | null>(null);
  let anchor = $state<{ fileId: string; digest: string; side: Side; line: number } | null>(null);
  let draft = $state('');
  let replies = $state<Record<string, string>>({});
  let selectedThreadId = $state<string | null>(null);
  const views = [
    { id: 'prep', label: 'Review prep' },
    { id: 'files', label: 'Files changed' },
    { id: 'conversations', label: 'Conversations' },
  ] as const;
  type WorkspaceView = (typeof views)[number]['id'];
  // Opening a review still lands directly on the diff, even while insights load.
  let activeView = $state<WorkspaceView>('files');
  const fileStatusLabels: Record<FileStatus, string> = {
    ignored: 'Ignored',
    'reviewed-current': 'Viewed',
    'changed-since-reviewed': 'Changed since review',
    unreviewed: 'Unreviewed',
  };

  let treeVisible = $state(true);
  let collapsedFolders = $state<Record<string, boolean>>({});
  let fileQuery = $state('');
  let fileCollapsed = $state(false);
  // Review insights (#38) load beside the review, never before it: the diff
  // renders from the session view alone and insights fill in as they arrive.
  let queue = $state<QueueView | null>(null);
  let findingItems = $state<FindingItem[] | null>(null);
  let insightsError = $state<string | null>(null);
  let insightsLoading = $state(false);
  let insightsRequest = 0;
  let navMode = $state<'tree' | 'queue'>('tree');
  let queueFilter = $state<QueueFilter>('remaining');
  let showIgnored = $state(false);
  let hideIgnoredInTree = $state(false);

  /** Fire-and-forget; read-only. Latest request wins. */
  function loadInsights() {
    const request = ++insightsRequest;
    insightsLoading = true;
    void Promise.allSettled([fetchQueue(), fetchFindings()]).then(([q, f]) => {
      if (request !== insightsRequest) return;
      insightsLoading = false;
      if (q.status === 'fulfilled') queue = q.value;
      if (f.status === 'fulfilled') findingItems = f.value.items;
      const failed = [q, f].find((r) => r.status === 'rejected') as PromiseRejectedResult | undefined;
      insightsError = failed ? (failed.reason instanceof Error ? failed.reason.message : String(failed.reason)) : null;
    });
  }

  onMount(() => {
    // A stacked sidebar would otherwise push the first diff below the viewport.
    treeVisible = !window.matchMedia?.('(max-width: 760px)').matches;
    return watchSession(
      (view) => { session = view; error = null; loadInsights(); },
      (failure) => { error = String(failure); },
    );
  });

  const file = $derived(session?.files.find((item) => item.id === selectedId) ?? session?.files[0]);
  const statesById = $derived(new Map(session?.fileStates.map((entry) => [entry.fileId, entry])));
  const queueById = $derived(new Map(queue?.queue.items.map((entry) => [entry.fileId, entry])));
  const fileState = $derived(file && statesById.get(file.id));
  const conversationCount = $derived(session ? session.threads.length + session.reviews.length + session.issueComments.length : 0);
  const threads = $derived(session?.threads.filter((thread) => thread.fileId === file?.id || thread.path === file?.path) ?? []);
  const sortedThreads = $derived(orderThreads(threads));
  const fileTree = $derived(buildFileTree(session?.files.filter((item) =>
    item.path.toLowerCase().includes(fileQuery.trim().toLowerCase())
      && (!hideIgnoredInTree || !statesById.get(item.id)?.scope.ignored),
  ) ?? []));
  const totals = $derived(
    session?.files.reduce(
      (acc, item) => ({ add: acc.add + item.additions, del: acc.del + item.deletions }),
      { add: 0, del: 0 },
    ) ?? { add: 0, del: 0 },
  );

  function selectFile(id: string) {
    selectedId = id;
    selectedThreadId = null;
    fileCollapsed = false;
    anchor = null;
    draft = '';
    actionError = null;
  }

  function openReview(id?: string) {
    const movingToDiff = activeView !== 'files' || window.matchMedia?.('(max-width: 760px)').matches;
    if (id && id !== file?.id) selectFile(id);
    if (id) fileCollapsed = false;
    activeView = 'files';
    if (movingToDiff) void tick().then(() => document.getElementById('file-review-heading')?.focus());
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
    fileCollapsed = false;
    activeView = 'files';
    void showThread(threadId);
  }

  const queueItem = $derived(file && queueById.get(file.id));

  function setScope(path: string, ignored: boolean) {
    // Rules only re-rank on the server; no analysis rerun is requested.
    void act(() => createFileRule(ignored ? 'exclude' : 'include', path));
  }

  function toggleViewed(checked: boolean) {
    if (!file || fileState?.scope.ignored) return;
    const { id, diffDigest } = file;
    void act(() => setReviewed(id, diffDigest, checked), () => {
      if (!checked || !session) return;
      const next = nextUnreviewedFile(session.files, session.fileStates, id);
      if (next) {
        if (fileQuery && !next.path.toLowerCase().includes(fileQuery.trim().toLowerCase())) fileQuery = '';
        selectFile(next.id);
        void tick().then(() => document.getElementById('file-review')?.scrollIntoView({ block: 'start' }));
      }
    });
  }

  function lineSide(line: DiffLine): Side | null {
    if (line.kind === 'meta') return null;
    return line.kind === 'deletion' ? 'LEFT' : 'RIGHT';
  }

  function selectLine(line: DiffLine) {
    if (!file || !file.patchComplete || fileState?.scope.ignored) return;
    const side = lineSide(line);
    const number = side === 'LEFT' ? line.oldLine : line.newLine;
    if (!side || number === null) return;
    anchor = { fileId: file.id, digest: file.diffDigest, side, line: number };
    draft = '';
    actionError = null;
  }

  async function showThread(threadId: string) {
    selectedThreadId = threadId;
    await tick();
    const card = document.getElementById(`thread-${threadId}`);
    card?.focus({ preventScroll: true });
    card?.scrollIntoView({ block: 'nearest' });
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
    const { fileId, digest, side, line } = anchor;
    const body = draft.trim();
    void act(() => createThread(fileId, digest, side, line, body), () => { anchor = null; draft = ''; });
  }

  function saveReply(threadId: string) {
    const body = replies[threadId]?.trim();
    if (!body) return;
    void act(() => replyToThread(threadId, body), () => { replies[threadId] = ''; });
  }

</script>

<div class="workspace">
  {#if !session}
    <header class="workspace-toolbar"><p class="font-semibold">Aside</p></header>
    {#if error}<p class="p-6 text-red-700" role="alert">{error}</p>
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
      <div class="flex shrink-0 flex-wrap items-center gap-2">
        <span class="workspace-status" class:is-open={session.pr.state.toLowerCase() === 'open' && !session.pr.isDraft}>
          <span class="status-dot" aria-hidden="true"></span>{session.pr.isDraft ? 'Draft' : session.pr.state.toLowerCase()}
        </span>
        {#if session.demo}<span class="workspace-chip">Demo · local only</span>{/if}
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
        {#if error}<p class="text-red-700" role="alert">Refresh failed: {error}</p>{/if}
        {#if actionError}<p class="text-red-700" role="alert">{actionError}</p>{/if}
        {#each session.warnings as warning}<p class="text-amber-800">{warning}</p>{/each}
      </div>
    {/if}

    {#snippet renderFile(item: ChangedFile)}
      {@const state = statesById.get(item.id)}
      {@const indicators = fileIndicators(queueById.get(item.id))}
      <li class="tree-row" class:is-current={file?.id === item.id} class:is-ignored={state?.scope.ignored}>
        <button type="button" onclick={() => openReview(item.id)} aria-current={file?.id === item.id ? 'true' : undefined}
          aria-label={`Review ${item.path}${indicators.length ? `. ${indicators.map((i) => i.label).join(', ')}` : ''}`} title={item.path}
          class="tree-file">
          <span class="tree-glyph" aria-hidden="true">▤</span>
          <span class="min-w-0">
            <span class="tree-filename block truncate font-mono">{item.path.split('/').at(-1)}</span>
            <span class="block text-xs text-muted">
              {fileStatusLabels[state?.status ?? 'unreviewed']} · +{item.additions} −{item.deletions}
            </span>
            {#if indicators.length}
              <span class="mt-1 flex flex-wrap gap-1" aria-hidden="true" data-testid="file-indicators">
                {#each indicators as indicator (indicator.key)}
                  <span class="workspace-chip" title={indicator.label}>{indicator.glyph} {indicator.short}</span>
                {/each}
              </span>
            {/if}
          </span>
        </button>
        <span class="status-dot" data-status={state?.status ?? 'unreviewed'} aria-hidden="true"></span>
        <button type="button" class="tree-scope" disabled={busy}
          aria-label={`${state?.scope.ignored ? 'Include' : 'Ignore'} file ${item.path}`}
          title={state?.scope.ignored ? 'Include file' : 'Ignore file'}
          onclick={() => setScope(item.path, !state?.scope.ignored)}>
          <span aria-hidden="true">{state?.scope.ignored ? '◉' : '◌'}</span>
        </button>
      </li>
    {/snippet}
    {#snippet renderFolder(folder: FileTreeNode)}
      <li>
        <button type="button" onclick={() => { collapsedFolders[folder.path] = !collapsedFolders[folder.path]; }}
          aria-expanded={!collapsedFolders[folder.path]} aria-label={`${collapsedFolders[folder.path] ? 'Expand' : 'Collapse'} ${folder.path}`}
          class="tree-folder">
          <span aria-hidden="true">{collapsedFolders[folder.path] ? '▸' : '▾'}</span>
          <span class="truncate" title={folder.path}>{folder.name}/</span>
        </button>
        {#if !collapsedFolders[folder.path]}
          <ul class="tree-children">
            {#each folder.folders as child (child.path)}{@render renderFolder(child)}{/each}
            {#each folder.files as item (item.id)}{@render renderFile(item)}{/each}
          </ul>
        {/if}
      </li>
    {/snippet}

    <div class="workspace-body" class:tree-collapsed={!treeVisible}>
      <aside class="workspace-sidebar" aria-label="Review file tree">
        <div class="sidebar-content">
          <div class="sidebar-heading">
            <h2>Changed files <span>{session.progress.inScope} / {session.files.length}</span></h2>
            <button type="button" class="workspace-button icon-button" onclick={() => { treeVisible = !treeVisible; }}
              aria-expanded={treeVisible} aria-controls="file-tree" aria-label={treeVisible ? 'Hide file tree' : 'Show file tree'}
              title={treeVisible ? 'Hide file tree' : 'Show file tree'}>
              <span aria-hidden="true">{treeVisible ? '‹' : '›'}</span>
            </button>
          </div>
          <nav id="file-tree" aria-label="Changed files" hidden={!treeVisible}>
            <p class="mb-3 text-xs text-muted">Ignore files that don’t need your attention. Scope rules are saved for this repository.</p>
            <div class="sidebar-modes" role="group" aria-label="File order">
              <button type="button" aria-pressed={navMode === 'tree'} onclick={() => { navMode = 'tree'; }}
                class="workspace-button focus-visible:outline-2">Path tree</button>
              <button type="button" aria-pressed={navMode === 'queue'} onclick={() => { navMode = 'queue'; }}
                class="workspace-button focus-visible:outline-2">Suggested order</button>
            </div>
            {#if navMode === 'tree'}
              <label class="mb-2 block">
                <span class="sr-only">Filter changed files</span>
                <input type="search" bind:value={fileQuery} placeholder="Filter files…" class="workspace-input w-full" />
              </label>
              <div class="mb-2 flex flex-wrap items-center justify-between gap-1 text-xs">
                <span class="workspace-eyebrow text-muted">Review scope</span>
                <button type="button" class="text-accent font-semibold hover:underline"
                  aria-pressed={hideIgnoredInTree}
                  onclick={() => { hideIgnoredInTree = !hideIgnoredInTree; }}>Hide ignored</button>
              </div>
              {#if fileTree.files.length || fileTree.folders.length}
                <ul class="sidebar-scroll">
                  {#each fileTree.folders as folder (folder.path)}{@render renderFolder(folder)}{/each}
                  {#each fileTree.files as item (item.id)}{@render renderFile(item)}{/each}
                </ul>
              {:else}
                <p class="py-2 text-xs text-muted">No files match this filter.</p>
              {/if}
            {:else}
              <ReviewQueue {queue} selectedId={file?.id} bind:filter={queueFilter} bind:showIgnored onSelect={openReview} />
            {/if}
          </nav>
        </div>
      </aside>

      <main class="workspace-main">
        <section id="workspace-prep" hidden={activeView !== 'prep'} aria-label="Review prep">
          <ReviewPrep {session} {queue} {insightsError} onOpenFile={openReview} onStart={startReview} />
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
            <button type="button" class="workspace-button" onclick={openPrep}>← Scope</button>
          </div>
          {#if file}
            <div class="workspace-review-grid">
              <section id="file-review" class="workspace-diff" aria-label="File review">
                <div class="workspace-diff-header">
                  <div class="flex min-w-0 items-center gap-2">
                    <button type="button" onclick={() => { fileCollapsed = !fileCollapsed; }} aria-expanded={!fileCollapsed} aria-label={`${fileCollapsed ? 'Expand' : 'Collapse'} diff for ${file.path}`}
                      class="workspace-button icon-button">{fileCollapsed ? '▸' : '▾'}</button>
                    <div class="min-w-0">
                      <h3 class="wrap-anywhere font-mono text-sm font-semibold">{file.path}</h3>
                      <p class="text-xs text-muted">
                        {file.changeKind} · {fileStatusLabels[fileState?.status ?? 'unreviewed']}
                        {#if file.previousPath} · from {file.previousPath}{/if}
                        {#if !file.patchComplete} · incomplete patch{/if}
                      </p>
                    </div>
                  </div>
                  <div class="workspace-diff-actions">
                    <span class="font-mono text-xs"><span class="text-added">+{file.additions}</span> <span class="text-deleted">−{file.deletions}</span></span>
                    <button type="button" disabled={busy} onclick={() => setScope(file.path, !fileState?.scope.ignored)}
                      aria-label={`${fileState?.scope.ignored ? 'Include in review' : 'Exclude from review'} ${file.path}`}
                      class="workspace-button">
                      {fileState?.scope.ignored ? 'Include in review' : 'Exclude from review'}
                    </button>
                    <label class="workspace-button" class:opacity-50={busy || fileState?.scope.ignored}>
                      <input type="checkbox" checked={fileState?.reviewStatus === 'reviewed-current'} disabled={busy || fileState?.scope.ignored}
                        onchange={(event) => { const next = event.currentTarget.checked; event.currentTarget.checked = fileState?.reviewStatus === 'reviewed-current'; toggleViewed(next); }}
                        class="accent-accent" /> Viewed
                    </label>
                  </div>
                </div>
                {#if !fileCollapsed}
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
                          <div class="diff-row flex min-w-max whitespace-pre" class:bg-added-soft={line.kind === 'addition'} class:bg-deleted-soft={line.kind === 'deletion'}>
                            <span class="w-12 shrink-0 select-none border-r border-line px-2 text-right text-muted">{line.oldLine ?? ''}</span>
                            <span class="w-12 shrink-0 select-none border-r border-line px-2 text-right text-muted">{line.newLine ?? ''}</span>
                            <span class="flex w-6 shrink-0 items-center justify-center">
                              {#if line.kind !== 'meta' && lineThreads.length}
                                <button type="button" onclick={() => void showThread(lineThreads[0].id)}
                                  aria-label={`Show ${lineThreads.length} ${lineThreads.length === 1 ? 'thread' : 'threads'} on ${lineThreads[0].side === 'LEFT' ? 'old' : 'new'} line ${lineThreads[0].line}`}
                                  aria-pressed={lineThreads.some((thread) => thread.id === selectedThreadId)}
                                  title={`${lineThreads.length} ${lineThreads.length === 1 ? 'thread' : 'threads'} on this line`}
                                  class="thread-marker"></button>
                              {/if}
                            </span>
                            {#if lineSide(line) && file.patchComplete && !fileState?.scope.ignored}
                              <button type="button" onclick={() => selectLine(line)} title="Draft a comment on this line" class="line-draft w-7 shrink-0 text-center text-accent hover:bg-accent-soft focus-visible:outline-2 focus-visible:outline-accent">+</button>
                            {:else}
                              <span class="w-7 shrink-0"></span>
                            {/if}
                            <span class="px-2">{line.kind === 'addition' ? '+' : line.kind === 'deletion' ? '-' : ' '}{line.text}</span>
                          </div>
                          {#if !fileState?.scope.ignored && anchor?.fileId === file.id && anchor.digest === file.diffDigest && anchor.side === lineSide(line) && anchor.line === (anchor.side === 'LEFT' ? line.oldLine : line.newLine)}
                            <div class="min-w-0 border-y border-line bg-white p-3 font-sans whitespace-normal">
                              <label for="draft" class="block text-sm font-semibold">Draft on {anchor.side === 'LEFT' ? 'old' : 'new'} line {anchor.line}</label>
                              <textarea id="draft" bind:value={draft} rows="3" class="workspace-input mt-2 w-full" placeholder="Write a review comment"></textarea>
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
                {/if}
                <p class="workspace-diff-footer">Drafts and review progress are saved locally.</p>
              </section>

              <aside class="workspace-context" aria-label="Review threads">
                <section class="workspace-panel" aria-label="Threads on selected file">
                  <div class="workspace-panel-heading">
                    <h3 class="text-sm font-semibold">Threads ({threads.length})</h3>
                  </div>
                  <div class="p-3">
                    {#if threads.length === 0}<p class="text-sm text-muted">No threads on this file yet. Select a diff line to draft one.</p>{/if}
                    {#each sortedThreads as thread (thread.id)}
                      <article id={'thread-' + thread.id} tabindex="-1" class="review-thread"
                        class:is-selected={selectedThreadId === thread.id}>
                        <p class="text-xs text-muted">
                          {#if thread.source === 'github'}<span class="mr-1 rounded-full border border-accent px-2 font-semibold text-ink"><span aria-hidden="true">❝</span> External feedback · GitHub</span>{:else}<span class="mr-1 rounded-full border border-line px-2 font-semibold text-ink">Aside · local</span>{/if}
                          {thread.side} line {thread.line ?? 'unknown'} · {thread.anchorState}{thread.resolved ? ' · resolved' : ''}
                        </p>
                        {#each thread.comments as comment (comment.id)}
                          <div class="mt-2 border-t border-line pt-2">
                            <p class="text-xs font-semibold">{comment.author} · {comment.publishState}</p>
                            <p class="mt-1 whitespace-pre-wrap break-words">{comment.body}</p>
                          </div>
                        {/each}
                        {#if !fileState?.scope.ignored}
                          <form class="mt-3" onsubmit={(event) => { event.preventDefault(); saveReply(thread.id); }}>
                            <label for={'reply-' + thread.id} class="text-xs font-semibold">Reply</label>
                            <textarea id={'reply-' + thread.id} rows="2" value={replies[thread.id] ?? ''} oninput={(event) => { replies[thread.id] = event.currentTarget.value; }} class="workspace-input mt-1 w-full" placeholder="Write a reply"></textarea>
                            <button type="submit" disabled={busy || !replies[thread.id]?.trim()} class="workspace-button mt-1">Save reply</button>
                          </form>
                        {/if}
                      </article>
                    {/each}
                  </div>
                </section>
                <InsightsPanel {file} summary={session.analysis} {queueItem} findings={findingItems} {threads}
                  loading={insightsLoading} onShowThread={(id) => void showThread(id)} />
              </aside>
            </div>
          {:else}
            <p class="workspace-panel p-4 text-sm text-muted">No changed files to review.</p>
          {/if}
        </section>

        <section id="workspace-conversations" hidden={activeView !== 'conversations'} aria-label="Conversations">
          <Conversations {session} onOpenThread={openConversation} onReview={() => openReview()} />
        </section>
      </main>
    </div>
  {/if}
</div>
