<script lang="ts">
  import type { SessionView } from '../api';
  import PrConversation from '../insights/PrConversation.svelte';

  let { session, onOpenThread, onReview }: {
    session: SessionView;
    onOpenThread: (fileId: string, threadId: string) => void;
    onReview: () => void;
  } = $props();

  const fileIds = $derived(new Set(session.files.map((file) => file.id)));
  const idsByPath = $derived(new Map(session.files.map((file) => [file.path, file.id])));
</script>

<div class="workspace-view-heading">
  <div>
    <p class="workspace-eyebrow">Review feedback</p>
    <h2>All the feedback, one place.</h2>
    <p class="workspace-subtitle">GitHub reviews and comments, alongside local review threads.</p>
  </div>
  <button type="button" class="workspace-button primary-button" onclick={onReview}>Review files <span aria-hidden="true">→</span></button>
</div>

{#if session.reviews.length || session.issueComments.length}
  <PrConversation reviews={session.reviews} comments={session.issueComments} />
{/if}

<h3 class="workspace-section-title">File threads ({session.threads.length})</h3>
{#if session.threads.length}
  <ul class="space-y-2">
    {#each session.threads as thread (thread.id)}
      {@const fileId = thread.fileId && fileIds.has(thread.fileId) ? thread.fileId : idsByPath.get(thread.path)}
      {@const comment = thread.comments[0]}
      {#snippet preview()}
        <span class="preview-mark" aria-hidden="true">◎</span>
        <span class="min-w-0 flex-1">
          <span class="block wrap-anywhere font-semibold">
            {comment?.author ?? 'Thread'} · <span class="font-mono">{thread.path}{thread.line === null ? '' : ':' + thread.line}</span>
          </span>
          {#if comment?.body}<span class="preview-description" class:line-clamp-2={!!fileId}>{comment.body}</span>{/if}
          <span class="preview-description">
            {thread.resolved ? 'Resolved' : 'Open'}{#if !fileId} · file not in current diff{/if}
          </span>
        </span>
        <span class="workspace-chip">{thread.source === 'github' ? 'GitHub' : 'Local'}</span>
      {/snippet}
      <li>
        {#if fileId}
          <button type="button" class="workspace-preview-card"
            aria-label={`Open ${thread.source === 'github' ? 'GitHub' : 'local'} thread by ${comment?.author ?? 'unknown author'} on ${thread.path}${thread.line === null ? '' : ':' + thread.line}`}
            onclick={() => onOpenThread(fileId, thread.id)}>{@render preview()}</button>
        {:else}
          <article class="workspace-preview-card">{@render preview()}</article>
        {/if}
      </li>
    {/each}
  </ul>
{:else}
  <p class="workspace-panel p-3 text-sm text-muted">No file threads yet.</p>
{/if}
