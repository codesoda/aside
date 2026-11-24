import type { FindingItem, QueueView, SessionView } from '../api';
import type { ReviewFinding } from '../generated/ReviewFinding';
import type { ReviewThread } from '../generated/ReviewThread';
import { describeReason, emptyFindingsMessage, isThreadProjection, verdictMessage } from '../insights';
import AnalysisStatus from '../insights/AnalysisStatus';
import { Button } from '../components/ui/button';

export default function ReviewPrep({ session, queue, findings = null, threads = [], insightsError, onOpenFile, onOpenThread, onStart }: {
  session: SessionView; queue: QueueView | null; findings?: FindingItem[] | null; threads?: ReviewThread[];
  insightsError: string | null; onOpenFile: (id: string, findingId?: string) => void;
  onOpenThread?: (fileId: string, threadId: string) => void; onStart: () => void;
}) {
  const candidates = new Set(session.fileStates.filter((state) => !state.scope.ignored && state.reviewStatus !== 'reviewed-current').map((state) => state.fileId));
  const suggested = queue?.queue.items.filter((item) => candidates.has(item.fileId)).slice(0, 3) ?? [];
  const byId = new Map<string, ReviewFinding>((findings ?? []).filter((item) => item.label === 'current').map((item) => [item.finding.id, item.finding]));
  return <>
    <div className="workspace-view-heading"><div>
      <p className="workspace-eyebrow">Pre-review · Scope &amp; signals</p>
      <h2 id="review-prep-heading" tabIndex={-1}>Start with the important bits.</h2>
      <p className="workspace-subtitle">Set your review scope and see which files deserve a closer look.</p>
    </div><Button type="button" className="workspace-button primary-button" onClick={onStart}>Start reviewing <span aria-hidden="true">→</span></Button></div>
    <dl className="workspace-stats"><div><dt>In scope</dt><dd>{session.progress.inScope}</dd></div><div><dt>Ignored</dt><dd>{session.progress.ignored}</dd></div><div><dt>Already reviewed</dt><dd>{session.progress.reviewed}</dd></div></dl>
    <h3 className="workspace-section-title">Suggested places to look</h3>
    {suggested.length ? <ul className="space-y-2">{suggested.map((item) => {
      const reasons = item.reasons.map((reason) => describeReason(reason, byId, threads));
      const attention = item.findings.filter((entry) => entry.current && entry.attention !== 'none' && entry.disposition !== 'dismissed_local' && entry.disposition !== 'snoozed' && byId.has(entry.id));
      return <li key={item.fileId}><article className="workspace-preview-card block" aria-label={`Suggested file ${item.path}`}><div className="flex items-start gap-2">
        <span className={`preview-mark${item.attention === 'high' ? ' attention' : ''}`} aria-hidden="true">↗</span><div className="min-w-0">
          <Button variant="link" type="button" className="h-auto p-0 wrap-anywhere font-mono font-semibold text-left whitespace-normal" aria-label={`Review suggested file ${item.path}`} onClick={() => onOpenFile(item.fileId)}>{item.path}</Button>
          <ul className="preview-description list-disc pl-4" aria-label={`Reasons for ${item.path}`}>{reasons.map((reason, i) => <li key={i}>{reason.text}{reason.cites.length ? ` — ${reason.cites.join(' · ')}` : ''}</li>)}</ul>
          {findings === null ? <p className="preview-description">{insightsError ? 'Findings unavailable.' : 'Loading findings…'}</p> : attention.length ? <>
            <ul className="mt-2 space-y-1" aria-label={`Attention findings for ${item.path}`}>{attention.slice(0, 3).map((entry) => {
              const finding = byId.get(entry.id)!;
              const projection = isThreadProjection(finding, threads) ? threads.find((thread) => thread.githubThreadId === finding.anchor.githubThreadId) : undefined;
              return <li key={entry.id}>{!projection || onOpenThread ? <Button variant="link" type="button" className="h-auto p-0 text-left whitespace-normal" onClick={() => projection ? onOpenThread?.(item.fileId, projection.id) : onOpenFile(item.fileId, entry.id)} aria-label={projection ? `Open thread for finding ${finding.title} in ${item.path}` : `Review finding ${finding.title} in ${item.path}`}>
                <span className="font-semibold">{finding.anchor.line === null ? 'File' : finding.anchor.startLine && finding.anchor.startLine !== finding.anchor.line ? `L${finding.anchor.startLine}–${finding.anchor.line}` : `L${finding.anchor.line}`}</span> — {finding.title}
              </Button> : <><span className="font-semibold">{finding.anchor.line === null ? 'File' : `L${finding.anchor.startLine ?? finding.anchor.line}`}</span> — {finding.title} · Native thread</>}</li>;
            })}</ul>{attention.length > 3 && <p className="text-xs text-muted">+{attention.length - 3} more</p>}
          </> : <p className="preview-description">{emptyFindingsMessage(session.analysis, 'attention findings in this file')}</p>}
        </div></div></article></li>;
    })}</ul> : <p className="workspace-panel p-3 text-sm text-muted">{queue ? verdictMessage(queue.queue.verdict) : insightsError ? 'Suggested order is unavailable.' : 'Loading suggested order…'}</p>}
    <AnalysisStatus summary={session.analysis} loadError={insightsError} />
  </>;
}
