import type { AnalysisSummary } from '../generated/AnalysisSummary';
import { coverageHeadline, coverageTone, sourceComplete, sourceStateText } from '../insights';

export default function AnalysisStatus({ summary, loadError = null }: { summary: AnalysisSummary; loadError?: string | null }) {
  const tone = coverageTone(summary);
  const toneText = { complete: 'complete', pending: 'running', incomplete: 'incomplete' } as const;
  return <section className="workspace-panel mt-4 p-3 text-sm" aria-label="Analysis status">
    <p role="status" aria-live="polite" data-testid="analysis-status" data-coverage={tone}>
      <span aria-hidden="true">{tone === 'complete' ? '●' : tone === 'pending' ? '◌' : '◐'}</span> Analysis {toneText[tone]}: {coverageHeadline(summary)}
    </p>
    {loadError && <p className="text-warning" role="alert">Insights unavailable: {loadError}</p>}
    <details className="mt-1"><summary className="cursor-pointer text-accent focus-visible:outline-2 focus-visible:outline-accent">Source coverage</summary>
      <ul className="mt-1 space-y-1">{summary.sources.map((source) => <li key={source.sourceId}>
        <span className="font-mono">{source.sourceId}</span>: <span className={!sourceComplete(source) ? 'font-semibold' : undefined}>{sourceStateText(source.state)}</span>
        {source.items.state === 'known' ? <> · {source.items.value} {source.itemKind}</> : <> · {source.itemKind} count unknown</>}
        {source.reasonCodes.length > 0 && <> · {source.reasonCodes.join(', ')}</>}
        {source.limitations.map((limitation, index) => <span key={index} className="block pl-4 text-warning">{limitation}</span>)}
      </li>)}</ul>
    </details>
  </section>;
}
