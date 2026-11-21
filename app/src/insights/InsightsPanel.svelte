<script lang="ts">
  import type { FindingItem } from '../api';
  import type { AnalysisSummary } from '../generated/AnalysisSummary';
  import type { ChangedFile } from '../generated/ChangedFile';
  import type { QueueItem } from '../generated/QueueItem';
  import type { ReviewFinding } from '../generated/ReviewFinding';
  import type { ReviewThread } from '../generated/ReviewThread';
  import { describeReason, emptyFindingsMessage, findingsForFile, isThreadProjection, type FindingFilter } from '../insights';
  import FindingCard from './FindingCard.svelte';

  let {
    file, summary, queueItem, findings, threads, loading, onShowThread,
  }: {
    file: ChangedFile;
    summary: AnalysisSummary;
    queueItem: QueueItem | undefined;
    /** `null` while the first load is in flight. */
    findings: FindingItem[] | null;
    threads: ReviewThread[];
    loading: boolean;
    onShowThread: (threadId: string) => void;
  } = $props();

  let filter = $state<FindingFilter>('all');
  const byId = $derived(new Map<string, ReviewFinding>((findings ?? []).map((item) => [item.finding.id, item.finding])));
  const fileFindings = $derived(findings
    ? findingsForFile(findings, file.id, file.path, filter).filter((item) => !isThreadProjection(item.finding, threads))
    : []);
  const reasons = $derived((queueItem?.reasons ?? []).map((reason) => describeReason(reason, byId, threads)));
  const filters: { value: FindingFilter; label: string }[] = [
    { value: 'all', label: 'All' },
    { value: 'measured', label: 'Measured' },
    { value: 'suggestion', label: 'Suggestions' },
    { value: 'external', label: 'External' },
  ];
</script>

<section class="workspace-panel" aria-labelledby="insights-heading">
  <div class="workspace-panel-heading">
    <h3 id="insights-heading" class="text-sm font-semibold">Review insights</h3>
  </div>
  <div class="p-3 text-sm">
    <h4 class="text-xs font-semibold uppercase text-muted">Why this file?</h4>
    {#if !queueItem}
      <p class="mt-1 text-muted" data-testid="insights-pending">{loading ? 'Loading suggested order…' : 'No queue entry for this file.'}</p>
    {:else}
      <ul class="mt-1 list-disc pl-5" aria-label={`Reasons for ${file.path}`}>
        {#each reasons as reason, i (i)}
          <li>
            {reason.text}{#if reason.cites.length}<span class="text-muted">{` — ${reason.cites.join(' · ')}`}</span>{/if}
            {#if reason.threadId}
              <button type="button" onclick={() => onShowThread(reason.threadId!)}
                class="ml-1 text-accent underline focus-visible:outline-2 focus-visible:outline-accent">Open thread</button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}

    <fieldset class="mt-3">
      <legend class="text-xs font-semibold uppercase text-muted">Findings</legend>
      <div class="mt-1 flex flex-wrap gap-1" role="group" aria-label="Filter findings by kind">
        {#each filters as option (option.value)}
          <button type="button" aria-pressed={filter === option.value} onclick={() => { filter = option.value; }}
            class="rounded-md border border-line px-2 py-0.5 text-xs hover:bg-surface focus-visible:outline-2 focus-visible:outline-accent"
            class:font-semibold={filter === option.value} class:bg-accent-soft={filter === option.value}>{option.label}</button>
        {/each}
      </div>
    </fieldset>
    <div class="mt-2">
      {#if findings === null}
        <p class="text-muted" data-testid="findings-pending">{loading ? 'Loading findings…' : 'Findings unavailable.'}</p>
      {:else if fileFindings.length === 0}
        <p class="text-muted" data-testid="findings-empty">{emptyFindingsMessage(summary, filter === 'all' ? 'this file' : `this file matching “${filters.find((f) => f.value === filter)?.label}”`)}</p>
      {:else}
        {#each fileFindings as item (item.finding.id)}<FindingCard {item} />{/each}
      {/if}
    </div>
  </div>
</section>
