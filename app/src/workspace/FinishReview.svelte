<script lang="ts">
  import { ApiError, confirmPublication, fetchSession, finishReview, previewPublication, refreshReview, type PublishOutcome, type PublishPreview, type SessionView, type Verdict } from '../api';

  let { session, onSession }: { session: SessionView; onSession: (view: SessionView) => void } = $props();
  let verdict = $state<Verdict>('COMMENT');
  let summary = $state('');
  let initialized = false;
  $effect(() => {
    if (initialized) return;
    verdict = session.review.verdict ?? 'COMMENT';
    summary = session.review.summary;
    initialized = true;
  });
  let preview = $state<PublishPreview | null>(null);
  let outcome = $state<PublishOutcome | null>(null);
  let error = $state('');
  let busy = $state(false);
  let stale = $state(false);
  const review = $derived(session.review);
  const planStale = $derived(stale || (preview !== null && (preview.plan.selectionRevision !== review.selectionRevision || preview.plan.headSha !== session.pr.headSha || preview.plan.baseSha !== session.pr.baseSha || preview.plan.verdict !== review.verdict || preview.plan.summary !== review.summary)));

  function invalidatePreview() {
    if (preview) stale = true;
  }

  async function showPreview() {
    if (busy) return;
    busy = true; error = ''; preview = null; outcome = null; stale = false;
    try {
      await finishReview(verdict, summary);
      onSession(await fetchSession());
      preview = await previewPublication();
    } catch (failure) {
      error = failure instanceof Error ? failure.message : String(failure);
    } finally { busy = false; }
  }

  async function confirm() {
    if (busy || !preview || preview.blockers.length || planStale || session.demo || review.unresolvedPublication) return;
    busy = true; error = '';
    try {
      const result = await confirmPublication(preview.plan.planId);
      outcome = result.outcome;
      preview = null;
      onSession(await fetchSession());
    } catch (failure) {
      if (failure instanceof ApiError && (failure.code === 'plan_changed' || failure.code === 'pr_changed' || failure.code === 'publish_blocked' || failure.code === 'publish_conflict')) {
        stale = true;
        error = failure.code === 'pr_changed' ? 'The pull request changed. Refresh it and make a new preview before publishing.'
          : failure.code === 'plan_changed' ? 'Drafts or selection changed. Make a new preview before publishing.'
          : 'Publication is blocked. Check GitHub for an earlier attempt; do not retry without checking.';
      } else {
        stale = true; // An unknown response may follow a remote write. Never offer a blind retry.
        error = 'The publication outcome is uncertain. Check GitHub before taking any further action.';
      }
      try { onSession(await fetchSession()); } catch { /* Keep the uncertain outcome visible. */ }
    } finally { busy = false; }
  }

  async function refresh() {
    if (busy) return;
    busy = true; preview = null; stale = true; error = '';
    try { onSession((await refreshReview()).session); }
    catch (failure) { error = failure instanceof Error ? failure.message : String(failure); }
    finally { busy = false; }
  }
</script>

<div class="workspace-view-heading"><div><p class="workspace-eyebrow">Finish review</p><h2>Choose what goes to GitHub.</h2>
  <p class="workspace-subtitle">Only a human can publish. Omitted drafts stay local. A partial or uncertain attempt is never retried automatically.</p></div></div>
<div class="workspace-panel p-4 space-y-4">
  {#if review.unresolvedPublication}
    <p role="alert" class="text-danger">Unresolved publication ({review.unresolvedPublication.status}, attempt {review.unresolvedPublication.attemptId}). Check GitHub. Aside will not retry or clear this attempt. Unsent drafts remain local.</p>
  {/if}
  <label class="block">Review verdict
    <select class="workspace-input block" bind:value={verdict} onchange={invalidatePreview} disabled={busy}>
      <option value="COMMENT">Comment</option><option value="APPROVE">Approve</option><option value="REQUEST_CHANGES">Request changes</option>
    </select>
  </label>
  <label class="block">Review summary
    <textarea class="workspace-input block w-full" rows="4" bind:value={summary} oninput={invalidatePreview} disabled={busy}></textarea>
  </label>
  <button type="button" class="workspace-button" onclick={showPreview} disabled={busy}>Finish and preview</button>
  {#if error}<p role="alert" class="text-danger">{error}</p>{/if}
  {#if planStale}<p role="status">This preview is stale. Make a new preview before publishing.</p><button type="button" class="workspace-button" onclick={refresh} disabled={busy}>Refresh pull request</button>{/if}
  {#if preview}
    <section aria-label="Publication preview" class="space-y-3">
      <h3 class="font-semibold">Exact publication preview</h3>
      <p>Verdict: {preview.plan.verdict} · Summary:</p><p class="whitespace-pre-wrap">{preview.plan.summary || '(empty)'}</p>
      {#if !preview.plan.inline.length && !preview.plan.replies.length}<p>No draft comments selected. This review sends the verdict and summary only.</p>{/if}
      <h4 class="font-semibold">Included destinations and bodies</h4>
      <ul class="space-y-2">
        {#each preview.plan.inline as item (item.commentId)}
          <li class="border border-line p-2">{item.path} · {item.side} {item.startLine && item.startLine !== item.line ? `lines ${item.startLine}–${item.line}` : `line ${item.line}`} · {item.authorKind}
            <p class="whitespace-pre-wrap break-words">{item.body}</p></li>
        {/each}
        {#each preview.plan.replies as item (item.commentId)}
          <li class="border border-line p-2">Reply in {item.path} · {item.side} line {item.line ?? 'unknown'} · parent {item.parentCommentId} · {item.authorKind}
            <p class="whitespace-pre-wrap break-words">{item.body}</p></li>
        {/each}
      </ul>
      <h4 class="font-semibold">Kept local / omitted drafts</h4>
      {#if !preview.plan.omitted.length}<p>None.</p>{/if}
      <ul>{#each preview.plan.omitted as item (item.commentId)}<li>{item.kind} {item.commentId} · {item.selected ? 'selected' : 'not selected'} · {item.code}: {item.reason}</li>{/each}</ul>
      {#if preview.blockers.length}
        <h4 class="font-semibold">Publication blockers</h4>
        <ul>{#each preview.blockers as blocker}<li role="alert">{blocker.code}: {blocker.message}</li>{/each}</ul>
      {/if}
      {#if session.demo}<p>Demo is local only. Publishing is unavailable.</p>{/if}
      <button type="button" class="workspace-button primary-button" onclick={confirm}
        disabled={busy || planStale || preview.blockers.length > 0 || session.demo || !!review.unresolvedPublication}>Publish to GitHub</button>
    </section>
  {/if}
  {#if outcome}
    <section aria-label="Publication result" class="space-y-2">
      <h3 class="font-semibold">Publication {outcome.status}</h3>
      <p>Published: {outcome.publishedCommentIds.join(', ') || 'none'}</p>
      <p>Remaining unsent: {outcome.remainingCommentIds.join(', ') || 'none'}</p>
      <p>Omitted: {outcome.omittedCommentIds.join(', ') || 'none'}</p>
      {#if outcome.status === 'partial'}<p role="alert">Check GitHub. Aside will not retry this attempt. Unsent drafts remain local.</p>{/if}
      {#if outcome.error}<p role="alert">{outcome.error}</p>{/if}
    </section>
  {/if}
</div>
