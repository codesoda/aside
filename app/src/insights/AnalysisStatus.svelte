<script lang="ts">
  import type { AnalysisSummary } from '../generated/AnalysisSummary';
  import { coverageHeadline, coverageTone, sourceComplete, sourceStateText } from '../insights';

  let { summary, loadError = null }: { summary: AnalysisSummary; loadError?: string | null } = $props();

  const tone = $derived(coverageTone(summary));
  const toneText = { complete: 'complete', pending: 'running', incomplete: 'incomplete' } as const;
</script>

<!-- Passive and polite: progress never steals focus from the diff. -->
<section class="workspace-panel mt-4 p-3 text-sm" aria-label="Analysis status">
  <p role="status" aria-live="polite" data-testid="analysis-status" data-coverage={tone}>
    <span aria-hidden="true">{tone === 'complete' ? '●' : tone === 'pending' ? '◌' : '◐'}</span>
    Analysis {toneText[tone]}: {coverageHeadline(summary)}
  </p>
  {#if loadError}<p class="text-warning" role="alert">Insights unavailable: {loadError}</p>{/if}
  <details class="mt-1">
    <summary class="cursor-pointer text-accent focus-visible:outline-2 focus-visible:outline-accent">Source coverage</summary>
    <ul class="mt-1 space-y-1">
      {#each summary.sources as source (source.sourceId)}
        <li>
          <span class="font-mono">{source.sourceId}</span>:
          <span class:font-semibold={!sourceComplete(source)}>{sourceStateText(source.state)}</span>
          {#if source.items.state === 'known'} · {source.items.value} {source.itemKind}{:else} · {source.itemKind} count unknown{/if}
          {#if source.reasonCodes.length} · {source.reasonCodes.join(', ')}{/if}
          {#each source.limitations as limitation}<span class="block pl-4 text-warning">{limitation}</span>{/each}
        </li>
      {/each}
    </ul>
  </details>
</section>
