<script lang="ts">
  import type { QueueView } from '../api';
  import { fileIndicators, filterQueue, QUEUE_FILTERS, reasonText, verdictMessage, type QueueFilter } from '../insights';

  let {
    queue, selectedId, filter = $bindable('remaining'), showIgnored = $bindable(false), onSelect,
  }: {
    queue: QueueView | null;
    selectedId: string | undefined;
    filter?: QueueFilter;
    showIgnored?: boolean;
    onSelect: (fileId: string) => void;
  } = $props();

  const items = $derived(queue ? filterQueue(queue.queue.items, filter, showIgnored) : []);
  const ignoredCount = $derived(queue?.counts.files.ignored ?? 0);
</script>

<div class="px-2 pb-2">
  <div class="flex flex-wrap items-center gap-2 p-2">
    <label class="text-xs font-semibold">Show
      <select bind:value={filter} class="ml-1 rounded-md border border-line px-1 py-1 text-xs font-normal focus-visible:outline-2 focus-visible:outline-accent">
        {#each QUEUE_FILTERS as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
      </select>
    </label>
    <label class="flex items-center gap-1 text-xs">
      <input type="checkbox" bind:checked={showIgnored} class="focus-visible:outline-2 focus-visible:outline-accent" />
      Reveal {ignoredCount} ignored {ignoredCount === 1 ? 'file' : 'files'}
    </label>
  </div>
  {#if !queue}
    <p class="p-2 text-sm text-muted" role="status">Suggested order is loading. The file tree and diff are ready.</p>
  {:else}
    <p class="px-2 text-xs text-muted" data-testid="queue-verdict">{verdictMessage(queue.queue.verdict)}</p>
    {#if items.length === 0}
      <p class="p-2 text-sm text-muted">No files match this filter.</p>
    {/if}
    <ol class="sidebar-scroll" aria-label="Suggested review order">
      {#each items as item (item.fileId)}
        {@const indicators = fileIndicators(item)}
        {@const why = item.reasons.map((r) => reasonText(r.code)).filter((t, i, all) => all.indexOf(t) === i)}
        <li>
          <button type="button" onclick={() => onSelect(item.fileId)} aria-current={selectedId === item.fileId ? 'true' : undefined}
            aria-label={`Review ${item.path}. ${why.join('; ')}${indicators.length ? `. ${indicators.map((i) => i.label).join(', ')}` : ''}`}
            class="block w-full min-w-0 rounded px-2 py-1.5 text-left text-sm hover:bg-surface focus-visible:outline-2 focus-visible:outline-accent"
            class:font-semibold={selectedId === item.fileId}
            class:bg-accent-soft={selectedId === item.fileId}>
            <span class="block truncate font-mono" title={item.path}>{item.path}</span>
            <span class="block text-xs text-muted">{why.join(' · ')}</span>
            {#if indicators.length}
              <span class="mt-0.5 flex flex-wrap gap-1 text-xs" aria-hidden="true">
                {#each indicators as indicator (indicator.key)}
                  <span class="rounded border border-line px-1" title={indicator.label}>{indicator.glyph} {indicator.short}</span>
                {/each}
              </span>
            {/if}
          </button>
        </li>
      {/each}
    </ol>
  {/if}
</div>
