<script lang="ts">
  import type { QueueView, SessionView } from '../api';
  import { reasonText, verdictMessage } from '../insights';
  import AnalysisStatus from '../insights/AnalysisStatus.svelte';

  let { session, queue, insightsError, onOpenFile, onStart }: {
    session: SessionView;
    queue: QueueView | null;
    insightsError: string | null;
    onOpenFile: (id: string) => void;
    onStart: () => void;
  } = $props();

  const candidates = $derived(new Set(session.fileStates
    .filter((state) => !state.scope.ignored && state.reviewStatus !== 'reviewed-current')
    .map((state) => state.fileId)));
  const suggested = $derived(queue?.queue.items.filter((item) => candidates.has(item.fileId)).slice(0, 3) ?? []);
</script>

<div class="workspace-view-heading">
  <div>
    <p class="workspace-eyebrow">Pre-review · Scope &amp; signals</p>
    <h2 id="review-prep-heading" tabindex="-1">Start with the important bits.</h2>
    <p class="workspace-subtitle">Set your review scope and see which files deserve a closer look.</p>
  </div>
  <button type="button" class="workspace-button primary-button" onclick={onStart}>Start reviewing <span aria-hidden="true">→</span></button>
</div>

<dl class="workspace-stats">
  <div><dt>In scope</dt><dd>{session.progress.inScope}</dd></div>
  <div><dt>Ignored</dt><dd>{session.progress.ignored}</dd></div>
  <div><dt>Already reviewed</dt><dd>{session.progress.reviewed}</dd></div>
</dl>

<h3 class="workspace-section-title">Suggested places to look</h3>
{#if suggested.length}
  <ul class="space-y-2">
    {#each suggested as item (item.fileId)}
      <li>
        <button type="button" class="workspace-preview-card" aria-label={`Review suggested file ${item.path}`} onclick={() => onOpenFile(item.fileId)}>
          <span class="preview-mark" class:attention={item.attention === 'high'} aria-hidden="true">↗</span>
          <span class="min-w-0">
            <span class="block wrap-anywhere font-mono font-semibold">{item.path}</span>
            <span class="preview-description">{[...new Set(item.reasons.map((r) => reasonText(r.code)))].join(' · ')}</span>
          </span>
        </button>
      </li>
    {/each}
  </ul>
{:else}
  <p class="workspace-panel p-3 text-sm text-muted">
    {queue ? verdictMessage(queue.queue.verdict) : insightsError ? 'Suggested order is unavailable.' : 'Loading suggested order…'}
  </p>
{/if}
<AnalysisStatus summary={session.analysis} loadError={insightsError} />
