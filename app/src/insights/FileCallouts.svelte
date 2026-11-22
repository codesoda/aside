<script lang="ts">
  import type { AnalysisSummary } from '../generated/AnalysisSummary';
  import type { FindingFilter } from '../insights';
  import { emptyFindingsMessage } from '../insights';
  import type { CalloutPlacements } from '../callouts';
  import FindingCard from './FindingCard.svelte';

  let { placements, summary, loading, findingsLoaded, filter = $bindable('all'), showLines = $bindable(true), selectedId, onReveal }: {
    placements: CalloutPlacements;
    summary: AnalysisSummary;
    loading: boolean;
    findingsLoaded: boolean;
    filter?: FindingFilter;
    showLines?: boolean;
    selectedId: string | null;
    onReveal: (id: string) => void;
  } = $props();
  const filters: { value: FindingFilter; label: string }[] = [
    { value: 'all', label: 'All' }, { value: 'measured', label: 'Measured' },
    { value: 'suggestion', label: 'Suggestions' }, { value: 'external', label: 'External' },
  ];
</script>

<div class="file-callouts" aria-labelledby="insights-heading">
  <div class="flex flex-wrap items-center gap-2 border-b border-line px-3 py-2">
    <h4 id="insights-heading" class="mr-2 text-sm font-semibold">Review insights</h4>
    <div class="flex flex-wrap gap-1" role="group" aria-label="Filter findings by kind">
      {#each filters as option (option.value)}
        <button type="button" class="workspace-button focus-visible:outline-2" aria-pressed={filter === option.value} onclick={() => filter = option.value}>{option.label}</button>
      {/each}
    </div>
    <button type="button" class="workspace-button focus-visible:outline-2" aria-pressed={showLines} onclick={() => showLines = !showLines}>{showLines ? 'Hide line callouts' : 'Show line callouts'}</button>
  </div>
  {#if !findingsLoaded}
    <p class="p-3 text-sm text-muted" data-testid="findings-pending">{loading ? 'Loading findings…' : 'Findings unavailable.'}</p>
  {:else if !placements.file.length && !placements.inline.length && !placements.unplaced.length}
    <p class="p-3 text-sm text-muted" data-testid="findings-empty">{emptyFindingsMessage(summary, filter === 'all' ? 'this file' : `this file matching “${filters.find((option) => option.value === filter)?.label}”`)}</p>
  {/if}
  {#if placements.file.length}
    <div class="callout-band" aria-label="Whole-file findings">
      {#each placements.file as callout (callout.item.finding.id)}
        <FindingCard item={callout.item} scopeLabel="Whole file" selected={selectedId === callout.item.finding.id} />
      {/each}
    </div>
  {/if}
  {#if placements.unplaced.length}
    <div class="callout-band" aria-label="Findings without a current diff row">
      {#each placements.unplaced as callout (callout.item.finding.id)}
        <FindingCard item={callout.item} selected={selectedId === callout.item.finding.id}
          note={`Not placed on a current diff row (${callout.reason.replaceAll('_', ' ')}). Original citation is retained.`} />
      {/each}
    </div>
  {/if}
  {#if !showLines && placements.inline.length}
    <div class="callout-band" aria-label="Hidden line callouts">
      <p class="mb-2 text-xs text-muted">{placements.inline.length} line {placements.inline.length === 1 ? 'callout' : 'callouts'} hidden. Markers remain in the diff.</p>
      {#each placements.inline as callout (callout.item.finding.id)}
        <button type="button" class="workspace-button mr-1 mb-1" onclick={() => onReveal(callout.item.finding.id)}>
          Show {callout.item.finding.title}
        </button>
      {/each}
    </div>
  {/if}
</div>
