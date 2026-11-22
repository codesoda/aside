<script lang="ts">
  import { fetchEvidence, type Evidence, type FindingItem } from '../api';
  import { citedLines, evidenceValue, presentAssertion, providerText, ruleText } from '../insights';

  let { item, scopeLabel, note, selected = false, collapsed = false, onDraft }: {
    item: FindingItem;
    scopeLabel?: string;
    note?: string;
    selected?: boolean;
    collapsed?: boolean;
    onDraft?: (item: FindingItem) => void;
  } = $props();

  const finding = $derived(item.finding);
  const kind = $derived(presentAssertion(finding.assertionKind, finding.validation.status));
  const rule = $derived(ruleText(finding));
  let open = $state(false);
  let detailsOpen = $state(false);
  let evidence = $state<Evidence[] | null>(null);
  let evidenceError = $state<string | null>(null);

  async function toggleEvidence() {
    open = !open;
    if (!open || evidence) return;
    evidenceError = null;
    try {
      evidence = await fetchEvidence(finding.id);
    } catch (failure) {
      evidenceError = failure instanceof Error ? failure.message : String(failure);
    }
  }

  // Border style and glyph carry the class as well as colour.
  const styles = {
    measured: 'border-solid border-accent bg-surface',
    suggestion: 'border-dashed border-warning bg-warning-soft',
    external: 'border-double border-4 border-accent bg-accent-soft',
  } as const;
</script>

<article id={`callout-${finding.id}`} tabindex="-1" data-callout-scope={scopeLabel ?? 'unplaced'}
  class={`mb-3 rounded-md border p-3 text-sm last:mb-0 ${styles[kind.cls]} ${selected ? 'ring-2 ring-accent' : ''}`}
  aria-label={`${kind.label}: ${finding.title}`} data-insight-class={kind.cls}>
  <p class="flex flex-wrap items-center gap-2 text-xs">
    <span class="rounded-full border border-current px-2 font-semibold" data-testid="assertion-label">
      <span aria-hidden="true">{kind.glyph}</span> {kind.label}
    </span>
    <span class="text-muted">{providerText(finding)}</span>
    {#if item.label === 'historical'}<span class="text-muted">· historical (from snapshot {item.latestSnapshotId})</span>{/if}
  </p>
  <h4 class="mt-2 font-semibold">{finding.title}</h4>
  <p class="mt-1 text-xs text-muted">{scopeLabel ?? citedLines(finding)} · {item.latestApplicability.replaceAll('_', ' ')}{#if rule} · rule {rule}{/if}</p>
  {#if note}<p class="mt-1 text-xs text-muted">{note}</p>{/if}
  {#if collapsed && finding.attention.level === 'none'}
    <button type="button" class="mt-2 text-xs font-semibold underline" onclick={() => detailsOpen = !detailsOpen} aria-expanded={detailsOpen}>Details</button>
  {/if}
  {#if !collapsed || finding.attention.level !== 'none' || detailsOpen}
    {#if finding.body}<p class="mt-2 whitespace-pre-wrap break-words">{finding.body}</p>{/if}
    {#if finding.rationale}<p class="mt-2 whitespace-pre-wrap break-words"><strong>Why:</strong> {finding.rationale}</p>{/if}
    {#if finding.suggestedAction}<p class="mt-2 whitespace-pre-wrap break-words"><strong>Suggested action:</strong> {finding.suggestedAction}</p>{/if}
  {/if}
  {#if finding.assertionKind === 'defect_hypothesis'}
    <p class="mt-1 text-xs">Validation: {finding.validation.status.replaceAll('_', ' ')}</p>
  {/if}
  {#if onDraft}<button type="button" class="mt-2 mr-2 rounded-md border border-line px-2 py-1 text-xs font-semibold" onclick={() => onDraft?.(item)}>Draft comment</button>{/if}
  <button type="button" onclick={() => void toggleEvidence()} aria-expanded={open} aria-controls={`evidence-${finding.id}`}
    aria-label={`${open ? 'Hide' : 'Show'} evidence for ${finding.title}`}
    class="mt-2 rounded-md border border-line bg-canvas px-2 py-1 text-xs font-semibold hover:bg-surface focus-visible:outline-2 focus-visible:outline-accent">
    {open ? 'Hide evidence' : 'Show evidence'}
  </button>
  {#if open}
    <div id={`evidence-${finding.id}`} class="mt-2 border-t border-line pt-2 text-xs">
      <dl class="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
        <dt class="font-semibold">Provider</dt><dd>{finding.source.providerId}{#if finding.source.runId} (run {finding.source.runId}){/if}</dd>
        <dt class="font-semibold">Rule</dt><dd>{rule ?? 'none recorded'}</dd>
        <dt class="font-semibold">Lines</dt><dd>{citedLines(finding)}</dd>
        {#if finding.rationale}<dt class="font-semibold">Why</dt><dd>{finding.rationale}</dd>{/if}
        {#if finding.attention.reasons.length}<dt class="font-semibold">Reasons</dt><dd>{finding.attention.reasons.join('; ')}</dd>{/if}
        {#if finding.suggestedAction}<dt class="font-semibold">To verify</dt><dd>{finding.suggestedAction}</dd>{/if}
        {#if finding.externalUrl}<dt class="font-semibold">Source</dt><dd><a class="text-accent underline" href={finding.externalUrl} rel="noreferrer noopener" target="_blank">Open original</a></dd>{/if}
      </dl>
      {#if evidenceError}
        <p role="alert" class="mt-2 text-danger">{evidenceError}</p>
      {:else if !evidence}
        <p class="mt-2 text-muted">Loading evidence…</p>
      {:else if evidence.length === 0}
        <p class="mt-2 text-muted">No evidence records were stored for this finding.</p>
      {:else}
        <ul class="mt-2 space-y-2" aria-label={`Evidence for ${finding.title}`}>
          {#each evidence as record (record.id)}
            <li class="rounded border border-line bg-canvas p-2">
              <p class="font-semibold">{record.label}</p>
              {#if evidenceValue(record)}<p>{evidenceValue(record)}</p>{/if}
              <p class="text-muted">{record.kind}{#if record.method} · method {record.method}{/if}{#if record.anchor} · {citedLines({ anchor: record.anchor })}{/if}</p>
              {#each record.limitations as limitation}<p class="text-warning">Limitation: {limitation}</p>{/each}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</article>
