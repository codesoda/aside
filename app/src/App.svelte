<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { ApiError, createThread, fetchSession, replyToThread, setReviewed, watchSession, type SessionView } from './api';
  import type { DiffLine } from './generated/DiffLine';
  import type { Side } from './generated/Side';
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
  let treeVisible = $state(true);
  let collapsedFolders = $state<Record<string, boolean>>({});
  let fileQuery = $state('');
  let fileCollapsed = $state(false);

  onMount(() => watchSession(
    (view) => { session = view; error = null; },
    (failure) => { error = String(failure); },
  ));

  const file = $derived(session?.files.find((item) => item.id === selectedId) ?? session?.files[0]);
  const fileState = $derived(session?.fileStates.find((item) => item.fileId === file?.id));
  const threads = $derived(session?.threads.filter((thread) => thread.fileId === file?.id || thread.path === file?.path) ?? []);
  const sortedThreads = $derived(orderThreads(threads));
  const fileTree = $derived(buildFileTree(session?.files.filter((item) => item.path.toLowerCase().includes(fileQuery.trim().toLowerCase())) ?? []));
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

<div class="min-h-screen bg-white font-sans text-[#1f2328]">
  <header class="flex items-center gap-3 border-b border-[#d1d9e0] bg-[#f6f8fa] px-6 py-3">
    <span class="font-semibold">Aside</span>
    {#if session?.demo}
      <span class="rounded-full border border-[#d1d9e0] px-2 text-xs text-[#59636e]">demo data · drafts stay local</span>
    {/if}
  </header>

  {#if error && !session}
    <p class="p-6 text-red-700" role="alert">{error}</p>
  {:else if !session}
    <p class="p-6 text-[#59636e]">Loading…</p>
  {:else}
    <main class="mx-auto max-w-[100rem] p-4 sm:p-6">
      <h1 class="text-2xl">
        {session.pr.title}
        <span class="font-light text-[#59636e]">#{session.pr.number}</span>
      </h1>
      <p class="mt-1 text-sm text-[#59636e]">
        {session.pr.author} wants to merge into
        <code class="rounded bg-[#ddf4ff] px-1 text-[#0969da]">{session.pr.baseRef}</code>
        from <code class="rounded bg-[#ddf4ff] px-1 text-[#0969da]">{session.pr.headRef}</code>
        · {session.pr.owner}/{session.pr.repo}
      </p>
      <p class="mt-3 text-sm text-[#59636e]">
        {session.progress.reviewed} of {session.progress.inScope} in-scope files reviewed
        · {session.progress.changedSinceReviewed} changed since review
        · {session.progress.ignored} ignored
        · <span class="text-[#1a7f37]">+{totals.add}</span>
        <span class="text-[#d1242f]">−{totals.del}</span>
      </p>
      {#if error}<p class="mt-3 text-red-700" role="alert">Refresh failed: {error}</p>{/if}
      {#if actionError}<p class="mt-3 text-red-700" role="alert">{actionError}</p>{/if}
      {#each session.warnings as warning}
        <p class="mt-2 text-sm text-amber-800">{warning}</p>
      {/each}

      {#snippet renderFile(item: ChangedFile)}
        {@const state = session?.fileStates.find((entry) => entry.fileId === item.id)}
        <li>
          <button type="button" onclick={() => selectFile(item.id)} aria-current={file?.id === item.id ? 'true' : undefined}
            aria-label={`Review ${item.path}`} title={item.path}
            class="block w-full min-w-0 rounded px-2 py-1.5 text-left text-sm hover:bg-[#f6f8fa] focus-visible:outline-2 focus-visible:outline-[#0969da]"
            class:font-semibold={file?.id === item.id}
            style:background-color={file?.id === item.id ? '#ddf4ff' : undefined}>
            <span class="block truncate font-mono">{item.path.split('/').at(-1)}</span>
            <span class="text-xs text-[#59636e]">{state?.status ?? 'unreviewed'} · +{item.additions} −{item.deletions}</span>
          </button>
        </li>
      {/snippet}
      {#snippet renderFolder(folder: FileTreeNode)}
        <li>
          <button type="button" onclick={() => { collapsedFolders[folder.path] = !collapsedFolders[folder.path]; }}
            aria-expanded={!collapsedFolders[folder.path]} aria-label={`${collapsedFolders[folder.path] ? 'Expand' : 'Collapse'} ${folder.path}`}
            class="flex w-full items-center gap-1 rounded px-2 py-1.5 text-left font-mono text-sm font-semibold hover:bg-[#f6f8fa] focus-visible:outline-2 focus-visible:outline-[#0969da]">
            <span aria-hidden="true" class="w-4 text-center">{collapsedFolders[folder.path] ? '▸' : '▾'}</span>
            <span class="truncate" title={folder.path}>{folder.name}</span>
          </button>
          {#if !collapsedFolders[folder.path]}
            <ul class="ml-3 border-l border-[#d1d9e0] pl-2">
              {#each folder.folders as child (child.path)}{@render renderFolder(child)}{/each}
              {#each folder.files as item (item.id)}{@render renderFile(item)}{/each}
            </ul>
          {/if}
        </li>
      {/snippet}

      <div class="mt-6 flex items-center justify-between gap-3">
        <button type="button" onclick={() => { treeVisible = !treeVisible; }} aria-expanded={treeVisible}
          class="rounded-md border border-[#d1d9e0] bg-white px-3 py-1.5 text-sm font-semibold hover:bg-[#f6f8fa] focus-visible:outline-2 focus-visible:outline-[#0969da]">
          {treeVisible ? 'Hide file tree' : 'Show file tree'}
        </button>
        <span class="text-sm text-[#59636e]">{session.progress.reviewed} / {session.progress.inScope} viewed</span>
      </div>
      <div class={treeVisible
        ? 'mt-4 grid gap-5 lg:grid-cols-[16rem_minmax(0,1fr)] xl:grid-cols-[15rem_minmax(0,1fr)_20rem]'
        : 'mt-4 grid gap-5 xl:grid-cols-[minmax(0,1fr)_20rem]'}>
        {#if treeVisible}
          <nav id="file-tree" class="min-w-0 rounded-md border border-[#d1d9e0] lg:sticky lg:top-4 lg:self-start" aria-label="Changed files">
            <h2 class="border-b border-[#d1d9e0] bg-[#f6f8fa] px-4 py-2 text-sm font-semibold">{session.files.length} changed files</h2>
            <label class="block p-2">
              <span class="sr-only">Filter changed files</span>
              <input type="search" bind:value={fileQuery} placeholder="Filter files…" class="w-full rounded-md border border-[#d1d9e0] px-2 py-1.5 text-sm focus-visible:outline-2 focus-visible:outline-[#0969da]" />
            </label>
            <ul class="max-h-[70vh] overflow-y-auto px-2 pb-2">
              {#each fileTree.folders as folder (folder.path)}{@render renderFolder(folder)}{/each}
              {#each fileTree.files as item (item.id)}{@render renderFile(item)}{/each}
            </ul>
          </nav>
        {/if}

        {#if file}
          <section id="file-review" class="min-w-0" aria-label="File review">
            <div class="sticky top-0 z-20 flex flex-wrap items-center justify-between gap-3 rounded-t-md border border-[#d1d9e0] bg-[#f6f8fa] px-4 py-3 shadow-sm">
              <div class="flex min-w-0 items-center gap-2">
                <button type="button" onclick={() => { fileCollapsed = !fileCollapsed; }} aria-expanded={!fileCollapsed} aria-label={`${fileCollapsed ? 'Expand' : 'Collapse'} diff for ${file.path}`}
                  class="rounded px-1 text-[#59636e] hover:bg-[#eaeef2] focus-visible:outline-2 focus-visible:outline-[#0969da]">{fileCollapsed ? '▸' : '▾'}</button>
                <div class="min-w-0">
                  <h2 class="break-all font-mono text-sm font-semibold">{file.path}</h2>
                  <p class="text-xs text-[#59636e]">
                    {file.changeKind} · {fileState?.status ?? 'unreviewed'}
                    {#if file.previousPath} · from {file.previousPath}{/if}
                    {#if !file.patchComplete} · incomplete patch{/if}
                  </p>
                </div>
              </div>
              <label class="flex cursor-pointer items-center gap-2 rounded-md border border-[#d1d9e0] bg-white px-3 py-1.5 text-sm font-semibold" class:opacity-50={busy || fileState?.scope.ignored}>
                <input type="checkbox" checked={fileState?.reviewStatus === 'reviewed-current'} disabled={busy || fileState?.scope.ignored}
                  onchange={(event) => { const next = event.currentTarget.checked; event.currentTarget.checked = fileState?.reviewStatus === 'reviewed-current'; toggleViewed(next); }}
                  class="accent-[#1f883d]" /> Viewed
              </label>
            </div>
            {#if !fileCollapsed}
            {#if fileState?.scope.ignored}
              <p class="border-x border-b border-[#d1d9e0] p-4 text-sm text-[#59636e]">Ignored by review scope. Read-only; this file does not count toward progress.</p>
            {/if}
            {#if file.isBinary || file.hunks.length === 0}
              <p class="border-x border-b border-[#d1d9e0] p-4 text-sm text-[#59636e]">{file.isBinary ? 'Binary file: no text diff.' : 'No text hunks available for this file.'}</p>
            {:else}
              <div class="overflow-x-auto border-x border-b border-[#d1d9e0] font-mono text-xs sm:text-sm">
                {#each file.hunks as hunk (hunk.id)}
                  <div class="min-w-max border-b border-[#d1d9e0] bg-[#ddf4ff] px-3 py-1 text-[#59636e]">@@ -{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},{hunk.newCount} @@ {hunk.header}</div>
                  {#each hunk.lines as line (line.id)}
                    {@const lineThreads = threadsForLine(line, threads)}
                    <div class="flex min-w-max whitespace-pre" class:bg-[#dafbe1]={line.kind === 'addition'} class:bg-[#ffebe9]={line.kind === 'deletion'}>
                      <span class="w-12 shrink-0 select-none border-r border-[#d1d9e0] px-2 text-right text-[#59636e]">{line.oldLine ?? ''}</span>
                      <span class="w-12 shrink-0 select-none border-r border-[#d1d9e0] px-2 text-right text-[#59636e]">{line.newLine ?? ''}</span>
                      <span class="flex w-6 shrink-0 items-center justify-center">
                        {#if line.kind !== 'meta' && lineThreads.length}
                          <button type="button" onclick={() => void showThread(lineThreads[0].id)}
                            aria-label={`Show ${lineThreads.length} ${lineThreads.length === 1 ? 'thread' : 'threads'} on ${lineThreads[0].side === 'LEFT' ? 'old' : 'new'} line ${lineThreads[0].line}`}
                            aria-pressed={lineThreads.some((thread) => thread.id === selectedThreadId)}
                            title={`${lineThreads.length} ${lineThreads.length === 1 ? 'thread' : 'threads'} on this line`}
                            class="h-4 w-4 rounded-full border-[3px] border-white bg-[#8250df] shadow-[0_0_0_1px_#6e40c9] hover:bg-[#6e40c9] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[#0969da]"
                            class:ring-2={lineThreads.some((thread) => thread.id === selectedThreadId)} class:ring-[#6e40c9]={lineThreads.some((thread) => thread.id === selectedThreadId)}></button>
                        {/if}
                      </span>
                      {#if lineSide(line) && file.patchComplete && !fileState?.scope.ignored}
                        <button type="button" onclick={() => selectLine(line)} title="Draft a comment on this line" class="w-7 shrink-0 text-center text-[#0969da] hover:bg-[#ddf4ff] focus-visible:outline-2 focus-visible:outline-[#0969da]">+</button>
                      {:else}
                        <span class="w-7 shrink-0"></span>
                      {/if}
                      <span class="px-2">{line.kind === 'addition' ? '+' : line.kind === 'deletion' ? '-' : ' '}{line.text}</span>
                    </div>
                    {#if anchor?.fileId === file.id && anchor.digest === file.diffDigest && anchor.side === lineSide(line) && anchor.line === (anchor.side === 'LEFT' ? line.oldLine : line.newLine)}
                      <div class="min-w-0 border-y border-[#d1d9e0] bg-white p-3 font-sans whitespace-normal">
                        <label for="draft" class="block text-sm font-semibold">Draft on {anchor.side === 'LEFT' ? 'old' : 'new'} line {anchor.line}</label>
                        <textarea id="draft" bind:value={draft} rows="3" class="mt-2 w-full rounded-md border border-[#d1d9e0] p-2 text-sm" placeholder="Write a review comment"></textarea>
                        <div class="mt-2 flex gap-2">
                          <button type="button" disabled={busy || !draft.trim()} onclick={saveDraft} class="rounded-md bg-[#1f883d] px-3 py-1.5 text-sm font-semibold text-white disabled:opacity-50">Save draft</button>
                          <button type="button" onclick={() => { anchor = null; draft = ''; }} class="rounded-md border border-[#d1d9e0] px-3 py-1.5 text-sm">Cancel</button>
                        </div>
                      </div>
                    {/if}
                  {/each}
                {/each}
              </div>
            {/if}
            {/if}
          </section>
          <aside class={treeVisible
            ? 'min-w-0 self-start rounded-md border border-[#d1d9e0] lg:col-start-2 xl:sticky xl:top-4 xl:col-start-3 xl:row-start-1 xl:max-h-[calc(100vh-2rem)] xl:overflow-y-auto'
            : 'min-w-0 self-start rounded-md border border-[#d1d9e0] xl:sticky xl:top-4 xl:col-start-2 xl:row-start-1 xl:max-h-[calc(100vh-2rem)] xl:overflow-y-auto'} aria-label="Review threads">
            <div class="border-b border-[#d1d9e0] bg-[#f6f8fa] px-4 py-3">
              <h3 class="text-sm font-semibold">Threads ({threads.length})</h3>
            </div>
            <div class="p-3">
              {#if threads.length === 0}<p class="text-sm text-[#59636e]">No threads on this file yet. Select a diff line to draft one.</p>{/if}
              {#each sortedThreads as thread (thread.id)}
                <article id={'thread-' + thread.id} tabindex="-1" class="mb-3 rounded-md border border-[#d1d9e0] p-3 text-sm last:mb-0 focus:outline-none"
                  class:border-[#8250df]={selectedThreadId === thread.id} class:bg-[#f8f6ff]={selectedThreadId === thread.id}>
                  <p class="text-xs text-[#59636e]">{thread.side} line {thread.line ?? 'unknown'} · {thread.anchorState}{thread.resolved ? ' · resolved' : ''} · {thread.source}</p>
                  {#each thread.comments as comment (comment.id)}
                    <div class="mt-2 border-t border-[#d1d9e0] pt-2">
                      <p class="text-xs font-semibold">{comment.author} · {comment.publishState}</p>
                      <p class="mt-1 whitespace-pre-wrap break-words">{comment.body}</p>
                    </div>
                  {/each}
                  {#if !fileState?.scope.ignored}
                    <form class="mt-3" onsubmit={(event) => { event.preventDefault(); saveReply(thread.id); }}>
                      <label for={'reply-' + thread.id} class="text-xs font-semibold">Reply</label>
                      <textarea id={'reply-' + thread.id} rows="2" value={replies[thread.id] ?? ''} oninput={(event) => { replies[thread.id] = event.currentTarget.value; }} class="mt-1 w-full rounded-md border border-[#d1d9e0] p-2" placeholder="Write a reply"></textarea>
                      <button type="submit" disabled={busy || !replies[thread.id]?.trim()} class="mt-1 rounded-md border border-[#d1d9e0] px-3 py-1.5 font-semibold disabled:opacity-50">Save reply</button>
                    </form>
                  {/if}
                </article>
              {/each}
            </div>
          </aside>
        {/if}
      </div>
    </main>
  {/if}
</div>
