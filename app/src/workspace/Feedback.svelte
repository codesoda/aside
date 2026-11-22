<script lang="ts">
  import type { FeedbackThread, NativeFeedbackRecord } from '../api';
  import type { Page } from '../generated/Page';
  import { anchorText, commentLink, label, link, nativeText, searchableRecord, searchableThread, sourceKind, threadLink, unresolvedByPath, value, nativeComment as nativeCommentFor, resolutionKnown } from '../feedback';

  let { threads, records, threadPage, recordPage, loading, errors, ignoredPaths, availablePaths, headSha, onOpenFile }:
    { threads: FeedbackThread[]; records: NativeFeedbackRecord[]; threadPage: Page<FeedbackThread> | null;
      recordPage: Page<NativeFeedbackRecord> | null; loading: boolean; errors: string[];
      ignoredPaths: Set<string>; availablePaths: Set<string>; headSha: string; onOpenFile: (path: string) => void } = $props();
  let query = $state('');
  let revealIgnored = $state(false);
  const counts = $derived(unresolvedByPath(threads));
  const ignoredUnresolved = $derived([...counts].filter(([path]) => ignoredPaths.has(path)).reduce((sum, [, count]) => sum + count, 0));
  const ignoredUnknown = $derived(threads.filter((thread) => ignoredPaths.has(thread.path) && !resolutionKnown(thread)).length);
  const matchedThreads = $derived(threads.filter((thread) => searchableThread(thread).includes(query.trim().toLowerCase()) && (!ignoredPaths.has(thread.path) || revealIgnored)));
  const matchedRecords = $derived(records.filter((record) => searchableRecord(record).includes(query.trim().toLowerCase())));
  const kinds = $derived([...new Set(records.map(sourceKind))]);
</script>

<section class="workspace-panel p-4" aria-label="Native GitHub feedback">
  <h2 class="text-lg font-semibold">GitHub feedback · read-only</h2>
  <p class="text-sm text-muted">Imported feedback is separate from Aside findings. Search covers all loaded pages, including replies. Search cannot cover GitHub data that was not imported.</p>
  {#if loading}<p role="status">Loading native feedback pages… Search results are incomplete.</p>{/if}
  {#each errors as error}<p role="alert" class="text-danger">Feedback load failed: {error}. Search results are incomplete.</p>{/each}
  <label class="mt-3 block text-sm font-semibold" for="feedback-search">Search imported feedback</label>
  <input id="feedback-search" type="search" bind:value={query} class="workspace-input mt-1 w-full" />
  <p class="mt-2 text-sm">{ignoredUnresolved} confirmed unresolved {ignoredUnresolved === 1 ? 'thread' : 'threads'} on ignored files · {ignoredUnknown} with resolution unknown{threadPage?.coverage.complete === false || !threadPage ? ' (known counts; coverage incomplete)' : ''}.</p>
  <button type="button" class="workspace-button mt-2" aria-pressed={revealIgnored} onclick={() => { revealIgnored = !revealIgnored; }}>
    {revealIgnored ? 'Hide ignored feedback' : 'Reveal ignored feedback'}
  </button>
  <p class="mt-2 text-xs text-muted">Reveal changes this view only. It does not include files in review or mark them viewed.</p>
  <div class="mt-4" aria-label="Feedback source coverage">
    <h3 class="font-semibold">Source coverage</h3>
    {#if !threadPage && !recordPage}<p>Coverage unavailable.</p>{/if}
    {#each [threadPage, recordPage] as page}
      {#if page}
        {#each page.coverage.sources as source (source.sourceId)}
          <p class="text-sm">{source.sourceId}: {source.state} · {source.completeness} · {source.itemKind} {source.items.state === 'known' ? source.items.value : 'count unknown'}{source.runId ? ` · run ${source.runId}` : ''}
            {#if source.limitations.length} · {source.limitations.join('; ')}{/if}</p>
        {/each}
        {#if !page.coverage.complete}<p class="text-warning text-sm">This source is incomplete or unavailable; empty results are not all clear.</p>{/if}
      {/if}
    {/each}
  </div>
  <section class="mt-5" aria-label="Imported review threads">
    <h3 class="font-semibold">Review threads and replies ({matchedThreads.length})</h3>
    {#if !loading && threadPage?.coverage.complete && threads.length === 0}<p>No imported review threads (complete source).</p>{/if}
    {#each matchedThreads as thread (thread.id)}
      {@const native = thread.github}
      {@const commit = native?.comments.find((comment) => comment.commitOid)?.commitOid}
      <article class="review-thread mt-2" data-feedback-thread={thread.id}>
        <p class="text-xs">GitHub thread · {resolutionKnown(thread) ? native?.isResolved ? 'Resolved' : 'Unresolved' : native?.restOnly ? 'Resolution unknown (REST-only)' : 'Resolution unknown'} · anchor {thread.anchorState}{thread.anchorReason ? ` · ${thread.anchorReason}` : ''} · created {thread.createdAt || 'unknown'}</p>
        {#if native}<p class="text-xs">Native anchor: {native.isOutdated ? 'outdated' : 'not marked outdated'} · {native.subjectType ?? 'subject unknown'} · original line {native.originalLine ?? 'unknown'}{native.resolvedBy ? ` · resolved by ${native.resolvedBy}` : ''}</p>{/if}
        <p class="text-sm">Anchor: {thread.path || 'Unknown'} · {thread.side} {thread.startLine && thread.line && thread.startLine !== thread.line ? `lines ${thread.startLine}–${thread.line}` : `line ${thread.line ?? 'unknown'}`} · {commit && headSha ? commit === headSha ? 'native commit matches current head (not necessarily refreshed)' : 'native commit differs from current head' : 'freshness unknown'}{commit ? ` · native commit ${commit}` : ''}{ignoredPaths.has(thread.path) ? ' · ignored file' : ''}</p>
        {#if threadLink(thread)}<a href={threadLink(thread)!} target="_blank" rel="noopener noreferrer" class="text-accent underline">Open thread on GitHub</a>{:else}<span class="text-xs text-muted">Thread source link unavailable</span>{/if}
        {#if availablePaths.has(thread.path)}<button type="button" class="workspace-button ml-2" onclick={() => onOpenFile(thread.path)}>Open {thread.path} in review</button>
        {:else}<p class="text-xs text-muted">No matching current file; this anchor cannot be opened in the review.</p>{/if}
        {#each thread.comments as comment (comment.id)}
          {@const nativeComment = nativeCommentFor(thread, comment)}
          <div class="mt-2 border-t border-line pt-2"><p class="text-xs">{comment.author || 'Unknown author'}{nativeComment?.authorType ? ` (${nativeComment.authorType})` : ''} · {comment.publishState} · {comment.createdAt || 'date unknown'} · updated {comment.updatedAt || 'unknown'}</p>
            {#if nativeComment}<p class="text-xs">Native comment anchor: {nativeComment.outdated ? 'outdated' : 'not marked outdated'} · line {nativeComment.line ?? 'unknown'} · original line {nativeComment.originalLine ?? 'unknown'}</p>{/if}<p class="whitespace-pre-wrap break-words">{comment.body}</p>
            {#if commentLink(thread, comment)}<a href={commentLink(thread, comment)!} target="_blank" rel="noopener noreferrer" class="text-accent underline">Open comment on GitHub</a>{:else}<span class="text-xs text-muted">Comment source link unavailable</span>{/if}
          </div>
        {/each}
      </article>
    {/each}
  </section>
  {#each kinds as kind (kind)}
    <section class="mt-5" aria-label={kind}>
      <h3 class="font-semibold">{kind} ({matchedRecords.filter((record) => sourceKind(record) === kind).length})</h3>
      {#each matchedRecords.filter((record) => sourceKind(record) === kind) as record, index (index)}
        {@const raw = value(record)}
        <article class="review-thread mt-2">
          <p class="text-xs">{kind} · {nativeText(record, 'author', 'appSlug', 'producer', 'context', 'name')} · {nativeText(record, 'state', 'status', 'conclusion', 'annotationLevel')}</p>
          <p class="text-sm">Anchor: {anchorText(record)} · freshness: {record.currentHead === true ? 'current head (not necessarily refreshed)' : record.currentHead === false ? 'historical head' : 'unknown'} · head {nativeText(record, 'headSha', 'sha', 'commitSha')} · {nativeText(record, 'updatedAt', 'completedAt', 'createdAt')}</p>
          {#if raw.annotationCoverage || record.annotationCoverage}<p class="text-sm">Annotation coverage: {JSON.stringify(raw.annotationCoverage ?? record.annotationCoverage)}</p>{/if}
          {#if raw.checkRunId || raw.githubCheckRunId || raw.runId}<p class="text-xs">Run: {label(String(raw.checkRunId ?? raw.githubCheckRunId ?? raw.runId))}{raw.annotationCoverage || record.annotationCoverage ? '' : ' · annotation coverage unknown'}</p>{/if}
          <p class="whitespace-pre-wrap break-words">{nativeText(record, 'body', 'message', 'summary', 'title')}</p>
          {#if link(record)}<a href={link(record)!} target="_blank" rel="noopener noreferrer" class="text-accent underline">Open source</a>{:else}<span class="text-xs text-muted">Source link unavailable</span>{/if}
          <details><summary>Native metadata</summary><pre class="overflow-auto text-xs">{JSON.stringify(record, null, 2)}</pre></details>
        </article>
      {/each}
    </section>
  {/each}
  {#if !loading && records.length === 0 && recordPage?.coverage.complete}<p>No other imported feedback (complete sources).</p>{/if}
</section>
