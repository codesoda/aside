<script lang="ts">
  import type { ReviewThread } from '../generated/ReviewThread';
  import { fetchPublicationStatus, reconcilePublication, type ActivePublication } from '../api';

  type Verdict = 'APPROVE' | 'REQUEST_CHANGES' | 'COMMENT';
  type PlannedInline = { commentId: string; threadId: string; path: string; side: 'LEFT' | 'RIGHT'; line: number; startLine: number | null; body: string };
  type PlannedReply = { commentId: string; threadId: string; parentCommentId: string; githubParentId: number | null; body: string };
  type OmittedDraft = { commentId: string; threadId: string; reason: string };
  export type Plan = { planId: string; verdict: Verdict; summary: string; inline: PlannedInline[]; replies: PlannedReply[]; omitted: OmittedDraft[]; preparedCount: number; openCount: number; resolvedCount: number; headSha: string; baseSha: string; attemptOrdinal: number };
  type Preview = { plan: Plan; sequence: number };
  type Outcome = { attemptId: string; status: string; githubReviewId: number | null; publishedCommentIds: string[]; remainingCommentIds: string[]; error: string | null };

  let { threads, disabledReason = null, demo = false, selfReview = false, onPreview, onSubmit, onLookup = fetchPublicationStatus, onReconcile = reconcilePublication, onReconciled }: {
    threads: ReviewThread[];
    disabledReason?: string | null;
    demo?: boolean;
    /** Only set when the viewer is known to be the PR author. */
    selfReview?: boolean;
    onPreview: (input: { verdict: Verdict; body: string }) => Promise<Preview>;
    onSubmit: (input: { planId: string; verdict: Verdict; body: string; confirm: true }) => Promise<{ outcome: Outcome; sequence: number }>;
    onLookup?: () => Promise<{ active: ActivePublication | null }>;
    onReconcile?: (input: { attemptId: string; outcome: 'posted' | 'not_posted'; confirm: true }) => Promise<{ sequence: number }>;
    onReconciled?: () => Promise<void>;
  } = $props();

  let open = $state(false);
  let verdict = $state<Verdict>('COMMENT');
  let body = $state('');
  let preview = $state<Preview | null>(null);
  let previewFingerprint = $state('');
  let error = $state('');
  let pending = $state(false);
  let locked = $state(true);
  let active = $state<ActivePublication | null>(null);
  let checked = $state(false);
  let transportUnknown = $state(false);
  let choice = $state<'posted' | 'not_posted' | ''>('');
  let choiceAttemptId = $state<string | null>(null);
  let result = $state<Outcome | null>(null);
  let trigger: HTMLButtonElement;
  let dialog = $state<HTMLElement>();
  let generation = 0;
  let lookupVersion = 0;
  const prepared = $derived(threads.filter((thread) => thread.reviewComment != null));
  const draftFingerprint = $derived(JSON.stringify(prepared.map((thread) => [thread.id, thread.reviewComment!.id, thread.reviewComment!.body])));
  const resolvedPrepared = $derived(prepared.filter((thread) => thread.resolved).length);
  const blocked = $derived(prepared.filter((thread) => thread.reviewComment?.blocker || thread.reviewComment?.locked));
  const reason = $derived(demo ? 'Review submission is unavailable in demo mode.' : disabledReason);
  const invalidBody = $derived(verdict !== 'APPROVE' && !body.trim());

  async function lookup() {
    const version = ++lookupVersion;
    try {
      const response = await onLookup();
      if (version !== lookupVersion) return;
      if (response.active?.attemptId !== active?.attemptId) { choice = ''; choiceAttemptId = null; }
      active = response.active;
      locked = !!active || transportUnknown;
      checked = true;
    } catch {
      if (version !== lookupVersion) return;
      locked = true;
      error = 'Could not check publication status. No new submission is safe until the status is available.';
    }
  }
  $effect(() => { void lookup(); });
  async function reconcile() {
    if (pending || !active?.attemptId || !choice || choiceAttemptId !== active.attemptId) return;
    pending = true; error = '';
    try {
      await onReconcile({ attemptId: active.attemptId, outcome: choice, confirm: true });
      await onReconciled?.();
      transportUnknown = false;
      await lookup();
      if (!checked || locked) error = 'Reconciliation was sent, but the current publication state could not be confirmed. Check again before submitting.';
      else { result = null; preview = null; choice = ''; }
    } catch (cause) {
      error = `${cause instanceof Error ? cause.message : 'Reconciliation failed.'} No further submission is safe; check GitHub and try reconciliation again.`;
    } finally { pending = false; }
  }
  function invalidate() { generation++; preview = null; previewFingerprint = ''; error = ''; result = null; }
  function close() {
    if (pending) return;
    open = false;
    if (!locked) invalidate();
    trigger?.focus();
  }
  function toggle() {
    if (open) { close(); return; }
    open = true;
    void lookup();
    if (!locked && result?.status !== 'succeeded') invalidate();
    setTimeout(() => dialog?.querySelector<HTMLElement>(locked ? 'button:not(:disabled)' : 'select')?.focus(), 0);
  }
  function keydown(event: KeyboardEvent) {
    if (event.key === 'Escape') { event.preventDefault(); close(); }
    if (event.key !== 'Tab' || !dialog) return;
    const items = [...dialog.querySelectorAll<HTMLElement>('button:not(:disabled), textarea:not(:disabled), select:not(:disabled)')];
    if (!items.length) return;
    if (event.shiftKey && document.activeElement === items[0]) { event.preventDefault(); items.at(-1)?.focus(); }
    else if (!event.shiftKey && document.activeElement === items.at(-1)) { event.preventDefault(); items[0].focus(); }
  }
  function validPlan(p: Preview): boolean {
    const plan = p?.plan;
    if (!plan || !plan.planId || plan.verdict !== verdict || plan.summary !== body || !Array.isArray(plan.inline) || !Array.isArray(plan.replies) || !Array.isArray(plan.omitted)) return false;
    const expected = new Set(prepared.map((thread) => thread.reviewComment!.id));
    const ids = [...plan.inline, ...plan.replies, ...plan.omitted].map((entry) => entry.commentId);
    return plan.preparedCount === expected.size && ids.length === expected.size && new Set(ids).size === ids.length && ids.every((id) => expected.has(id));
  }
  async function makePreview() {
    if (pending || locked || !checked || reason || invalidBody || (verdict === 'APPROVE' && selfReview)) return;
    pending = true; error = ''; result = null; preview = null;
    const fingerprint = draftFingerprint;
    const revision = ++generation;
    try {
      const response = await onPreview({ verdict, body });
      if (revision !== generation) return;
      if (fingerprint !== draftFingerprint || !validPlan(response)) { error = 'Preview does not match the current prepared comments or review text. Refresh and preview again.'; return; }
      previewFingerprint = fingerprint;
      preview = response;
    } catch (cause) { if (revision === generation) error = cause instanceof Error ? cause.message : 'Preview failed. No review was submitted.'; }
    finally { pending = false; }
  }
  async function submit() {
    if (pending || locked || !checked || reason || !preview || previewFingerprint !== draftFingerprint || !validPlan(preview) || invalidBody || (verdict === 'APPROVE' && selfReview)) return;
    const planId = preview.plan.planId;
    pending = true; preview = null; error = '';
    ++lookupVersion;
    try {
      const response = await onSubmit({ planId, verdict, body, confirm: true });
      result = response.outcome;
      if (response.outcome.status !== 'succeeded' && response.outcome.status !== 'rejected') {
        active = { attemptId: response.outcome.attemptId, affectedDraftIds: [...response.outcome.publishedCommentIds, ...response.outcome.remainingCommentIds] };
      }
      if (response.outcome.status === 'succeeded') {
        body = '';
        verdict = 'COMMENT';
        previewFingerprint = '';
        open = false;
        trigger?.focus();
      }
      else if (response.outcome.status === 'rejected') {
        error = response.outcome.error || 'Submission was rejected. Your draft is preserved; preview again before submitting.';
      } else {
        locked = true;
        error = response.outcome.error || 'Submission is partial or uncertain. Check GitHub before taking further action. Do not retry automatically.';
      }
    } catch (cause) {
      // Transport failures may mean GitHub accepted the review. Fail closed.
      locked = true;
      transportUnknown = true;
      checked = false;
      error = `${cause instanceof Error ? cause.message : 'Submission outcome unknown.'} Check GitHub before taking further action. Do not retry automatically.`;
      await lookup();
      locked = true; // A transport error is ambiguous even if no active claim was found.
      if (!active) error += ' No active attempt could be identified; do not guess an attempt ID or resubmit.';
    } finally { pending = false; }
  }
</script>

<div class="relative min-w-0 max-w-full">
  <button bind:this={trigger} type="button" aria-label={`Submit review — Draft (${prepared.length})`} aria-expanded={open} disabled={!!reason} title={reason || undefined} onclick={toggle}>
    Draft ({prepared.length}) · Submit review
  </button>
  {#if reason}<span class="mt-1 block wrap-anywhere" role="note">{reason}</span>{/if}
  {#if locked && !open}<span class="mt-1 block wrap-anywhere" role="alert">Submission outcome needs checking on GitHub before another attempt. Open review details to inspect it.</span>{/if}
  {#if result?.status === 'succeeded' && !open}<span class="mt-1 block wrap-anywhere" role="status">Review submitted successfully.</span>{/if}
  {#if open}
    <div bind:this={dialog} role="dialog" aria-modal="true" tabindex="-1" class="popover absolute right-0 z-50 w-96 max-w-[95vw] rounded border border-line bg-canvas p-4 shadow-lg" aria-label="Submit review" onkeydown={keydown}>
      <h2>Submit review</h2>
      <p>{prepared.length} prepared review comments · {resolvedPrepared} on resolved threads · {threads.filter((thread) => thread.resolved).length} resolved threads</p>
      {#if resolvedPrepared}<p role="note">Prepared comments on resolved threads remain in the submission plan unless blocked.</p>{/if}
      {#if blocked.length}<ul aria-label="Blocked prepared comments">{#each blocked as thread (thread.id)}<li>{thread.path}: {thread.reviewComment?.blocker || 'Draft reserved by an unresolved attempt'}</li>{/each}</ul>{/if}
      <label>Verdict
        <select aria-label="Review verdict" bind:value={verdict} onchange={invalidate} disabled={pending || locked}>
          <option value="COMMENT">Comment</option>
          <option value="REQUEST_CHANGES">Request changes</option>
          <option value="APPROVE" disabled={selfReview}>Approve</option>
        </select>
      </label>
      {#if selfReview}<p role="note">You cannot approve your own pull request.</p>{/if}
      <label>Overall comment
        <textarea aria-label="Overall comment" bind:value={body} oninput={invalidate} disabled={pending || locked}></textarea>
      </label>
      {#if invalidBody}<p>Overall comment is required for this verdict.</p>{/if}
      <button type="button" onclick={makePreview} disabled={pending || locked || !!reason || invalidBody || (verdict === 'APPROVE' && selfReview)}>Preview exact submission</button>
      {#if preview && previewFingerprint === draftFingerprint}
        <section aria-label="Exact outgoing preview">
          <h3>Exact outgoing preview</h3>
          <p>Verdict: {preview.plan.verdict} · Overall comment: {preview.plan.summary || '(none)'}</p>
          <p>{preview.plan.inline.length} inline comments · {preview.plan.replies.length} replies · {preview.plan.omitted.length} blocked</p>
          <ul>{#each preview.plan.inline as item (item.commentId)}<li>{item.path}:{item.line} ({item.side}) — {item.body}</li>{/each}</ul>
          <ul>{#each preview.plan.replies as item (item.commentId)}<li>Reply to {item.threadId}: {item.body}</li>{/each}</ul>
          <ul>{#each preview.plan.omitted as item (item.commentId)}<li>Not sent ({item.threadId}): {item.reason}</li>{/each}</ul>
          <button type="button" onclick={submit} disabled={pending || locked || !!reason}>Confirm and submit review</button>
        </section>
      {/if}
      {#if locked}
        <section aria-label="Resolve uncertain publication">
          <h3>Check GitHub before continuing</h3>
          <p>The review may already be posted. Do not retry the submission. Inspect the review and comments on GitHub, then explicitly confirm what happened.</p>
          {#if active?.attemptId}
            <p>Attempt: {active.attemptId}</p>
            {#if active.affectedDraftIds.length}<p>Affected draft IDs: {active.affectedDraftIds.join(', ')}</p>{/if}
            <label>What did you verify on GitHub?
              <select aria-label="Verified publication outcome" bind:value={choice} onchange={() => { choiceAttemptId = active?.attemptId ?? null; }} disabled={pending}>
                <option value="">Select only after checking GitHub</option>
                <option value="posted">Review posted</option>
                <option value="not_posted">Review did not post</option>
              </select>
            </label>
            <button type="button" onclick={reconcile} disabled={pending || !choice}>Confirm checked outcome</button>
          {:else}<p>No authoritative active attempt ID is available. Reconciliation is unavailable; do not submit again.</p>{/if}
        </section>
      {/if}
      {#if error}<p role="alert">{error}</p>{/if}
      {#if result}<p role="status">Status: {result.status}. Attempt: {result.attemptId}. {result.githubReviewId ? `GitHub review: ${result.githubReviewId}.` : ''} {result.publishedCommentIds.length} published; {result.remainingCommentIds.length} remaining. {result.error || ''}</p>{/if}
      <button type="button" onclick={close} disabled={pending}>Close</button>
    </div>
  {/if}
</div>
