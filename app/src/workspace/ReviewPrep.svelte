<script lang="ts">
  import type { FindingItem, QueueView, SessionView } from '../api';
  import type { ReviewFinding } from '../generated/ReviewFinding';
  import type { ReviewThread } from '../generated/ReviewThread';
  import { citedLines, describeReason, emptyFindingsMessage, isThreadProjection, presentAssertion, providerText, verdictMessage } from '../insights';
  import AnalysisStatus from '../insights/AnalysisStatus.svelte';

  let { session, queue, findings = null, threads = [], insightsError, onOpenFile, onOpenThread, onStart }: {
    session: SessionView;
    queue: QueueView | null;
    /** Null until findings load; omitted by older callers. */
    findings?: FindingItem[] | null;
    threads?: ReviewThread[];
    insightsError: string | null;
    onOpenFile: (id: string, findingId?: string) => void;
    onOpenThread?: (fileId: string, threadId: string) => void;
    onStart: () => void;
  } = $props();

  const candidates = $derived(new Set(session.fileStates
    .filter((state) => !state.scope.ignored && state.reviewStatus !== 'reviewed-current')
    .map((state) => state.fileId)));
  const suggested = $derived(queue?.queue.items.filter((item) => candidates.has(item.fileId)).slice(0, 3) ?? []);
  const byId = $derived(new Map<string, ReviewFinding>((findings ?? []).filter((item) => item.label === 'current').map((item) => [item.finding.id, item.finding])));
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
      {@const reasons = item.reasons.map((reason) => describeReason(reason, byId, threads))}
      {@const attention = item.findings.filter((entry) => entry.current && entry.attention !== 'none' && entry.disposition !== 'dismissed_local' && entry.disposition !== 'snoozed' && byId.has(entry.id))}
      <li>
        <article class="workspace-preview-card block" aria-label={`Suggested file ${item.path}`}>
          <div class="flex items-start gap-2">
            <span class="preview-mark" class:attention={item.attention === 'high'} aria-hidden="true">↗</span>
            <div class="min-w-0">
              <button type="button" class="wrap-anywhere font-mono font-semibold text-left hover:underline focus-visible:outline-2 focus-visible:outline-accent" aria-label={`Review suggested file ${item.path}`} onclick={() => onOpenFile(item.fileId)}>{item.path}</button>
              <ul class="preview-description list-disc pl-4" aria-label={`Reasons for ${item.path}`}>
                {#each reasons as reason, i (i)}
                  <li>{reason.text}{#if reason.cites.length}{` — ${reason.cites.join(' · ')}`}{/if}</li>
                {/each}
              </ul>
              {#if findings === null}
                <p class="preview-description">{insightsError ? 'Findings unavailable.' : 'Loading findings…'}</p>
              {:else if attention.length}
                <ul class="mt-2 space-y-1" aria-label={`Attention findings for ${item.path}`}>
                  {#each attention.slice(0, 3) as entry (entry.id)}
                    {@const finding = byId.get(entry.id)!}
                    {@const assertion = presentAssertion(finding.assertionKind, finding.validation.status)}
                    {@const projection = isThreadProjection(finding, threads) ? threads.find((thread) => thread.githubThreadId === finding.anchor.githubThreadId) : undefined}
                    <li>
                      {#if !projection || onOpenThread}
                        <button type="button" class="text-left hover:underline focus-visible:outline-2 focus-visible:outline-accent"
                          onclick={() => projection ? onOpenThread?.(item.fileId, projection.id) : onOpenFile(item.fileId, entry.id)}
                          aria-label={projection ? `Open thread for finding ${finding.title} in ${item.path}` : `Review finding ${finding.title} in ${item.path}`}>
                          <span class="font-semibold">{finding.title}</span>
                          <span class="block text-xs">{assertion.label} · {citedLines(finding)} · {providerText(finding)}</span>
                          {#if finding.rationale}<span class="block text-xs text-muted">{finding.rationale}</span>{/if}
                        </button>
                      {:else}
                        <span class="font-semibold">{finding.title}</span>
                        <span class="block text-xs">{assertion.label} · {citedLines(finding)} · {providerText(finding)} · Native thread</span>
                      {/if}
                    </li>
                  {/each}
                </ul>
                {#if attention.length > 3}<p class="text-xs text-muted">+{attention.length - 3} more</p>{/if}
              {:else}
                <p class="preview-description">{emptyFindingsMessage(session.analysis, 'attention findings in this file')}</p>
              {/if}
            </div>
          </div>
        </article>
      </li>
    {/each}
  </ul>
{:else}
  <p class="workspace-panel p-3 text-sm text-muted">
    {queue ? verdictMessage(queue.queue.verdict) : insightsError ? 'Suggested order is unavailable.' : 'Loading suggested order…'}
  </p>
{/if}
<AnalysisStatus summary={session.analysis} loadError={insightsError} />
