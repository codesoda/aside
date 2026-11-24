<script lang="ts">
  import type { FindingItem } from '../api';
  import FindingCard from './FindingCard.svelte';

  let { findings, loading, historicalFindings, historicalLoading, historicalLoadError, onHistorical, onHistoricalRetry, pending, error, loadError = null, onRetry, onState, onPromote, onOpenDraft, findingsWithDrafts = new Set<string>() }: {
    findings: FindingItem[] | null;
    loading: boolean;
    historicalFindings: FindingItem[] | null;
    historicalLoading: boolean;
    historicalLoadError: string | null;
    onHistorical: () => void;
    onHistoricalRetry: () => void;
    findingsWithDrafts?: ReadonlySet<string>;
    pending: string | null;
    error: string | null;
    loadError?: string | null;
    onRetry?: () => void;
    onState: (item: FindingItem, disposition: 'untouched' | 'dismissed_local' | 'snoozed', snoozedUntil?: string) => void;
    onPromote: (item: FindingItem) => void;
    onOpenDraft: (item: FindingItem) => void;
  } = $props();
  type Bucket = 'open' | 'dismissed_local' | 'snoozed' | 'historical';
  let bucket = $state<Bucket>('open');
  let snoozeDays = $state(1);
  const activeLoading = $derived(bucket === 'historical' ? historicalLoading : loading);
  const activeError = $derived(bucket === 'historical' ? historicalLoadError : loadError);
  const activeFindings = $derived(bucket === 'historical' ? historicalFindings : findings);
  const visible = $derived((activeFindings ?? []).filter((item) => bucket === 'historical'
    ? item.label === 'historical'
    : item.label === 'current' && (bucket === 'open' ? !['dismissed_local', 'snoozed'].includes(item.disposition) : item.disposition === bucket)));
</script>

<section class="workspace-panel p-3" aria-label="Browse findings">
  <h3 class="text-sm font-semibold">Browse findings</h3>
  <p class="mt-1 text-xs text-muted">Dismissed and snoozed findings stay available here. Historical findings are read-only.</p>
  <div class="mt-2 flex flex-wrap gap-2" role="group" aria-label="Finding status">
    {#each [['open', 'Open'], ['dismissed_local', 'Dismissed'], ['snoozed', 'Snoozed'], ['historical', 'Historical']] as option}
      <button type="button" class="workspace-button" aria-pressed={bucket === option[0]} onclick={() => { bucket = option[0] as Bucket; if (bucket === 'historical') onHistorical(); }}>{option[1]}</button>
    {/each}
  </div>
  {#if bucket === 'historical'}
    <p class="mt-2 text-xs text-muted">Past snapshots only. These findings cannot be changed or promoted.</p>
  {/if}
  {#if error}<p role="alert" class="mt-2 text-danger">{error}</p>{/if}
  {#if activeLoading}<p class="mt-2 text-sm text-muted">Loading findings…</p>
  {:else if activeError}
    <div class="mt-2" role="alert">
      <p>Could not load findings: {activeError}</p>
      <button type="button" class="workspace-button" onclick={bucket === 'historical' ? onHistoricalRetry : onRetry}>Retry finding load</button>
    </div>
  {:else if activeFindings === null && bucket === 'historical'}
    <button type="button" class="workspace-button mt-2" onclick={onHistorical}>Load historical findings</button>
  {:else if activeFindings === null}<p class="mt-2 text-sm text-muted">Waiting for findings from the current snapshot…</p>
  {:else if !visible.length}<p class="mt-2 text-sm text-muted">No {bucket.replace('_local', '').replace('_', ' ')} findings.</p>
  {:else}
    <ul class="mt-3 space-y-3">
      {#each visible as item (item.finding.id)}
        <li>
          <FindingCard {item} />
          {#if item.label === 'current'}
            <div class="mb-3 flex flex-wrap items-center gap-2">
              {#if bucket !== 'open'}<button type="button" class="workspace-button" disabled={pending !== null} onclick={() => onState(item, 'untouched')}>Restore {item.finding.title}</button>{/if}
              {#if bucket !== 'dismissed_local'}<button type="button" class="workspace-button" disabled={pending !== null} onclick={() => onState(item, 'dismissed_local')}>Dismiss {item.finding.title}</button>{/if}
              {#if bucket !== 'snoozed'}
                <label class="text-xs">Snooze for <select class="workspace-input" bind:value={snoozeDays} aria-label={`Snooze duration for ${item.finding.title}`}><option value={1}>1 day</option><option value={7}>7 days</option><option value={30}>30 days</option></select></label>
                <button type="button" class="workspace-button" disabled={pending !== null} onclick={() => onState(item, 'snoozed', new Date(Date.now() + Number(snoozeDays) * 86400000).toISOString())}>Snooze {item.finding.title}</button>
              {:else if item.userState?.snoozedUntil}<span class="text-xs text-muted">Until {new Date(item.userState.snoozedUntil).toLocaleString()}</span>{/if}
              {#if findingsWithDrafts.has(item.finding.id)}<button type="button" class="workspace-button" onclick={() => onOpenDraft(item)}>Open existing draft</button>
              {:else if item.canPromote}<button type="button" class="workspace-button" disabled={pending !== null} onclick={() => onPromote(item)}>Promote to draft</button>{/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>
