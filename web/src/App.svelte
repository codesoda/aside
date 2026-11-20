<script lang="ts">
  import { onMount } from 'svelte';
  import { ApiError, createThread, fetchSession, replyToThread, setReviewed, watchSession, type SessionView } from './api';
  import type { DiffLine } from './generated/DiffLine';
  import type { Side } from './generated/Side';

  let session = $state<SessionView | null>(null);
  let error = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let busy = $state(false);
  let selectedId = $state<string | null>(null);
  let anchor = $state<{ fileId: string; digest: string; side: Side; line: number } | null>(null);
  let draft = $state('');
  let replies = $state<Record<string, string>>({});

  onMount(() => watchSession(
    (view) => { session = view; error = null; },
    (failure) => { error = String(failure); },
  ));

  const file = $derived(session?.files.find((item) => item.id === selectedId) ?? session?.files[0]);
  const fileState = $derived(session?.fileStates.find((item) => item.fileId === file?.id));
  const threads = $derived(session?.threads.filter((thread) => thread.fileId === file?.id || thread.path === file?.path) ?? []);
  const totals = $derived(
    session?.files.reduce(
      (acc, item) => ({ add: acc.add + item.additions, del: acc.del + item.deletions }),
      { add: 0, del: 0 },
    ) ?? { add: 0, del: 0 },
  );

  function selectFile(id: string) {
    selectedId = id;
    anchor = null;
    draft = '';
    actionError = null;
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
    <main class="mx-auto max-w-7xl p-4 sm:p-6">
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

      <div class="mt-6 grid gap-5 lg:grid-cols-[18rem_minmax(0,1fr)]">
        <nav class="min-w-0 rounded-md border border-[#d1d9e0] lg:self-start" aria-label="Changed files">
          <h2 class="border-b border-[#d1d9e0] bg-[#f6f8fa] px-4 py-2 text-sm font-semibold">{session.files.length} changed files</h2>
          <ul class="max-h-[70vh] overflow-y-auto">
            {#each session.files as item (item.id)}
              {@const state = session.fileStates.find((entry) => entry.fileId === item.id)}
              <li class="border-b border-[#d1d9e0] last:border-b-0">
                <button type="button" onclick={() => selectFile(item.id)} aria-current={file?.id === item.id ? 'true' : undefined}
                  class:font-semibold={file?.id === item.id}
                  class="block w-full min-w-0 px-3 py-2 text-left text-sm hover:bg-[#f6f8fa] focus-visible:outline-2 focus-visible:outline-[#0969da]"
                  style:background-color={file?.id === item.id ? '#ddf4ff' : undefined}>
                  <span class="block break-all font-mono">{item.path}</span>
                  <span class="text-xs text-[#59636e]">{state?.status ?? 'unreviewed'} · +{item.additions} −{item.deletions}</span>
                </button>
              </li>
            {/each}
          </ul>
        </nav>

        {#if file}
          <section class="min-w-0" aria-label="File review">
            <div class="flex flex-wrap items-center justify-between gap-3 rounded-t-md border border-[#d1d9e0] bg-[#f6f8fa] px-4 py-3">
              <div class="min-w-0">
                <h2 class="break-all font-mono text-sm font-semibold">{file.path}</h2>
                <p class="text-xs text-[#59636e]">
                  {file.changeKind} · {fileState?.status ?? 'unreviewed'}
                  {#if file.previousPath} · from {file.previousPath}{/if}
                  {#if !file.patchComplete} · incomplete patch{/if}
                </p>
              </div>
              {#if !fileState?.scope.ignored}
                <button type="button" disabled={busy} onclick={() => void act(() => setReviewed(file.id, file.diffDigest, fileState?.reviewStatus !== 'reviewed-current'))}
                  class="rounded-md border border-[#d1d9e0] bg-white px-3 py-1.5 text-sm font-semibold hover:bg-[#f3f4f6] disabled:opacity-50">
                  {fileState?.reviewStatus === 'reviewed-current' ? 'Unmark reviewed' : 'Mark reviewed'}
                </button>
              {/if}
            </div>
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
                    <div class="flex min-w-max whitespace-pre" class:bg-[#dafbe1]={line.kind === 'addition'} class:bg-[#ffebe9]={line.kind === 'deletion'}>
                      <span class="w-12 shrink-0 select-none border-r border-[#d1d9e0] px-2 text-right text-[#59636e]">{line.oldLine ?? ''}</span>
                      <span class="w-12 shrink-0 select-none border-r border-[#d1d9e0] px-2 text-right text-[#59636e]">{line.newLine ?? ''}</span>
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

            <section class="mt-5" aria-label="Review threads">
              <h3 class="mb-2 font-semibold">Threads ({threads.length})</h3>
              {#if threads.length === 0}<p class="text-sm text-[#59636e]">No threads on this file yet. Select a diff line to draft one.</p>{/if}
              {#each threads as thread (thread.id)}
                <article class="mb-3 rounded-md border border-[#d1d9e0] p-3 text-sm">
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
            </section>
          </section>
        {/if}
      </div>
    </main>
  {/if}
</div>
