<script lang="ts">
  import type { ConversationThread } from '../api';
  import type { FindingItem } from '../api';
  import { deleteReviewComment, patchThread, replyToThread, retryAgent, saveReviewComment } from '../api';

  export type ConversationDraft = { reply: string; outgoing: string; editing: boolean };
  let { thread, drafts, finding, mobile = false, agentConnected = true, onRefresh, onError, onClose }: {
    thread: ConversationThread;
    drafts: Record<string, ConversationDraft>;
    finding?: FindingItem;
    mobile?: boolean;
    agentConnected?: boolean;
    onRefresh: () => Promise<void>;
    onError: (message: string) => void;
    onClose: () => void;
  } = $props();
  let pending = $state<Record<string, boolean>>({});
  let busy = $derived(pending[thread.id] ?? false);
  let draft = $derived(drafts[thread.id]);
  let reply = $derived(draft?.reply ?? '');
  let outgoing = $derived(draft?.outgoing ?? thread.reviewComment?.body ?? '');
  let editing = $derived(draft?.editing ?? false);
  function updateDraft(change: Partial<ConversationDraft>) {
    drafts[thread.id] = { reply, outgoing, editing, ...change };
  }
  function completeDraft(id: string, field: 'reply' | 'outgoing', submitted: string, change: Partial<ConversationDraft>) {
    const current = drafts[id];
    if (current?.[field] === submitted) drafts[id] = { ...current, ...change };
  }
  async function action(id: string, operation: () => Promise<void>) {
    if (pending[id]) return;
    pending[id] = true;
    try { await operation(); await onRefresh(); }
    catch (error) { if (thread.id === id) onError(error instanceof Error ? error.message : String(error)); }
    finally { pending[id] = false; }
  }
  function sendReply() {
    const id = thread.id;
    const submitted = reply;
    const body = submitted.trim();
    if (!body || thread.agent?.state === 'waiting' || thread.agent?.state === 'responding') return;
    void action(id, async () => { await replyToThread(id, body); completeDraft(id, 'reply', submitted, { reply: '' }); });
  }
  function copyAgent(body: string, commentId: string) {
    if (outgoing.trim() && outgoing !== body && !confirm('Replace the current outgoing review comment with this agent reply?')) return;
    const id = thread.id;
    updateDraft({ outgoing: body, editing: true });
    void action(id, () => saveReviewComment(id, body, commentId));
  }
</script>

<article id={(mobile ? 'thread-mobile-' : 'thread-') + thread.id} tabindex="-1" class="review-thread is-selected" aria-label="Selected conversation">
  <div class="flex items-start justify-between gap-2">
    <div><p class="text-xs text-muted">{thread.source === 'github' ? 'GitHub feedback' : 'Private conversation'} · {thread.subject === 'file' || thread.line === null ? 'File' : `${thread.side} ${thread.startLine && thread.startLine !== thread.line ? `lines ${thread.startLine}–${thread.line}` : `line ${thread.line}`}`} · {thread.anchorState}</p>
      {#if finding}<h3 class="mt-2 font-semibold">{finding.finding.title}</h3><p class="mt-1 text-sm">{finding.finding.rationale}</p>{/if}
    </div>
    <button type="button" class="workspace-button" aria-label="Close conversation" onclick={onClose}>×</button>
  </div>
  {#each thread.comments as comment (comment.id)}
    <div class="mt-3 border-t border-line pt-2">
      <p class="text-xs font-semibold">{comment.author} · {comment.publishState}</p>
      <p class="mt-1 whitespace-pre-wrap break-words">{comment.body}</p>
      {#if comment.authorKind === 'agent' && (comment.publishState as string) === 'private'}
        <button type="button" class="workspace-button mt-2" disabled={busy} onclick={() => copyAgent(comment.body, comment.id)} title="Copy verbatim to outgoing review comment">+ Add to review</button>
      {/if}
    </div>
  {/each}
  {#if thread.agent?.state === 'waiting' || thread.agent?.state === 'responding'}
    <p role="status" class="mt-3 text-sm">Agent {thread.agent.state}<span class="agent-dots" aria-hidden="true">…</span></p>
  {:else if thread.agent?.state === 'failed'}
    <p role="status" class="mt-3 text-sm">Agent failed{thread.agent.error ? `: ${thread.agent.error}` : ''}.
      {#if thread.agent?.requestCommentId}<button type="button" class="workspace-button" disabled={busy} onclick={() => { const id = thread.id; const requestId = thread.agent!.requestCommentId!; void action(id, () => retryAgent(id, requestId)); }}>Retry</button>{/if}
    </p>
  {:else if thread.agent?.state === 'no_agent' || !agentConnected}<p role="status" class="mt-3 text-sm text-muted">No agent connected.</p>{/if}
  <form class="mt-3" onsubmit={(event) => { event.preventDefault(); sendReply(); }}>
    <label for={'reply-' + (mobile ? 'mobile-' : 'desktop-') + thread.id} class="text-xs font-semibold">Private reply</label>
    <textarea id={'reply-' + (mobile ? 'mobile-' : 'desktop-') + thread.id} rows="2" value={reply} oninput={(event) => updateDraft({ reply: event.currentTarget.value })} class="workspace-input mt-1 w-full" placeholder="Discuss privately" onkeydown={(event) => {
      if (event.key === 'Enter' && (event.metaKey || event.ctrlKey) && !event.isComposing && !event.repeat) { event.preventDefault(); sendReply(); }
    }}></textarea>
    {#if thread.agent?.state === 'waiting' || thread.agent?.state === 'responding'}<p class="text-xs text-muted">Wait for the current agent response before sending another reply.</p>{/if}
    <button type="submit" disabled={busy || !reply.trim() || thread.agent?.state === 'waiting' || thread.agent?.state === 'responding'} class="workspace-button mt-1">Send private reply</button>
  </form>
  {#if thread.source === 'local'}
    <div class="mt-3 flex gap-2">
      <button type="button" class="workspace-button" disabled={busy} onclick={() => { const id = thread.id; const resolved = !thread.resolved; void action(id, () => patchThread(id, { resolved })); }}>{thread.resolved ? 'Reopen locally' : 'Mark handled locally'}</button>
    </div>
  {/if}
  {#if thread.source === 'local' || thread.source === 'github'}
    <section class="mt-4 border-t border-line pt-3" aria-label="Outgoing review comment">
      <div class="flex items-center justify-between gap-2"><h4 class="font-semibold">Outgoing review comment</h4>
        {#if thread.reviewComment}<button type="button" class="workspace-button" disabled={busy} aria-label="Clear outgoing review comment" onclick={() => { const id = thread.id; const submitted = outgoing; void action(id, async () => { await deleteReviewComment(id); completeDraft(id, 'outgoing', submitted, { outgoing: '', editing: false }); }); }}>× Clear</button>{/if}
      </div>
      <p class="text-xs text-muted">{thread.source === 'github' ? 'This draft replies to the GitHub thread when you submit the review. It does not resolve the upstream thread.' : 'This draft is submitted as a review comment at its file anchor.'} Private replies are never included.</p>
      {#if thread.reviewComment?.target}<p class="text-xs text-muted">Target: {thread.reviewComment.target.kind === 'reply' ? 'GitHub thread reply' : thread.reviewComment.target.kind === 'file_fallback' ? 'file comment' : 'inline comment'} · {thread.reviewComment.target.path}{thread.reviewComment.target.line === null ? '' : `:${thread.reviewComment.target.line}`}</p>{/if}
      {#if thread.reviewComment?.blocker}<p role="status" class="text-xs text-muted">Cannot submit yet: {thread.reviewComment.blocker}</p>{/if}
      {#if editing || !thread.reviewComment}
        <textarea aria-label="Review comment draft" rows="3" value={outgoing} oninput={(event) => { const value = event.currentTarget.value; updateDraft({ outgoing: value }); if (!value && thread.reviewComment && !busy) { const id = thread.id; void action(id, async () => { await deleteReviewComment(id); completeDraft(id, 'outgoing', value, { outgoing: '', editing: true }); }); } }} class="workspace-input mt-2 w-full" placeholder="Write an outgoing review comment"></textarea>
        <button type="button" class="workspace-button mt-1" disabled={busy || !outgoing.trim()} onclick={() => { const id = thread.id; const submitted = outgoing; const body = submitted.trim(); void action(id, async () => { await saveReviewComment(id, body); completeDraft(id, 'outgoing', submitted, { outgoing: body, editing: false }); }); }}>Save review comment</button>
      {:else}
        <button type="button" class="mt-2 block w-full rounded border border-line p-2 text-left hover:bg-accent-soft focus-visible:outline-2 focus-visible:outline-accent" onclick={() => { updateDraft({ outgoing: thread.reviewComment?.body ?? '', editing: true }); }} aria-label="Edit outgoing review comment">{thread.reviewComment.body}</button>
      {/if}
    </section>
  {/if}
</article>

<style>
  .agent-dots { display: inline-block; animation: pulse 1s infinite alternate; }
  @keyframes pulse { from { opacity: .2; } to { opacity: 1; } }
  @media (prefers-reduced-motion: reduce) { .agent-dots { animation: none; } }
</style>
