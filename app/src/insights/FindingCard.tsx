import { useEffect, useRef, useState } from 'react';
import { fetchEvidence, type Evidence, type FindingItem } from '../api';
import { citedLines, evidenceValue, presentAssertion, providerText, ruleText } from '../insights';
import { Button } from '../components/ui/button';

export type FindingCardProps = {
  item: FindingItem;
  scopeLabel?: string;
  note?: string;
  selected?: boolean;
  collapsed?: boolean;
  onDraft?: (item: FindingItem) => void;
};

export default function FindingCard({ item, scopeLabel, note, selected = false, collapsed = false, onDraft }: FindingCardProps) {
  const finding = item.finding;
  const kind = presentAssertion(finding.assertionKind, finding.validation.status);
  const rule = ruleText(finding);
  const [open, setOpen] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [evidence, setEvidence] = useState<Evidence[] | null>(null);
  const [evidenceError, setEvidenceError] = useState<string | null>(null);
  const request = useRef(0);
  useEffect(() => { request.current++; setOpen(false); setEvidence(null); setEvidenceError(null); setDetailsOpen(false); return () => { request.current++; }; }, [finding.id]);
  async function toggleEvidence() {
    setOpen(!open);
    if (open || evidence) return;
    setEvidenceError(null);
    const current = ++request.current;
    try { const result = await fetchEvidence(finding.id); if (request.current === current) setEvidence(result); }
    catch (failure) { if (request.current === current) setEvidenceError(failure instanceof Error ? failure.message : String(failure)); }
  }
  const styles = { measured: 'border-solid border-accent bg-surface', suggestion: 'border-dashed border-warning bg-warning-soft', external: 'border-double border-4 border-accent bg-accent-soft' } as const;
  return <article id={`callout-${finding.id}`} tabIndex={-1} data-callout-scope={scopeLabel ?? 'unplaced'} className={`mb-3 rounded-md border p-3 text-sm last:mb-0 ${styles[kind.cls]} ${selected ? 'ring-2 ring-accent' : ''}`} aria-label={`${kind.label}: ${finding.title}`} data-insight-class={kind.cls}>
    <p className="flex flex-wrap items-center gap-2 text-xs"><span className="rounded-full border border-current px-2 font-semibold" data-testid="assertion-label"><span aria-hidden="true">{kind.glyph}</span> {kind.label}</span><span className="text-muted">{providerText(finding)}</span>{item.label === 'historical' && <span className="text-muted">· historical (from snapshot {item.latestSnapshotId})</span>}</p>
    <h4 className="mt-2 font-semibold">{finding.title}</h4>
    <p className="mt-1 text-xs text-muted">{scopeLabel ?? citedLines(finding)} · {item.latestApplicability.replaceAll('_', ' ')}{rule && <> · rule {rule}</>}</p>
    {note && <p className="mt-1 text-xs text-muted">{note}</p>}
    {collapsed && finding.attention.level === 'none' && <Button type="button" variant="link" className="mt-2 text-xs" onClick={() => setDetailsOpen(!detailsOpen)} aria-expanded={detailsOpen}>Details</Button>}
    {(!collapsed || finding.attention.level !== 'none' || detailsOpen) && <>{finding.body && <p className="mt-2 whitespace-pre-wrap break-words">{finding.body}</p>}{finding.rationale && <p className="mt-2 whitespace-pre-wrap break-words"><strong>Why:</strong> {finding.rationale}</p>}{finding.suggestedAction && <p className="mt-2 whitespace-pre-wrap break-words"><strong>Suggested action:</strong> {finding.suggestedAction}</p>}</>}
    {finding.assertionKind === 'defect_hypothesis' && <p className="mt-1 text-xs">Validation: {finding.validation.status.replaceAll('_', ' ')}</p>}
    {onDraft && <Button type="button" variant="outline" size="sm" className="mt-2 mr-2" onClick={() => onDraft(item)}>Draft comment</Button>}
    <Button type="button" variant="outline" size="sm" onClick={() => void toggleEvidence()} aria-expanded={open} aria-controls={`evidence-${finding.id}`} aria-label={`${open ? 'Hide' : 'Show'} evidence for ${finding.title}`} className="mt-2">{open ? 'Hide evidence' : 'Show evidence'}</Button>
    {open && <div id={`evidence-${finding.id}`} className="mt-2 border-t border-line pt-2 text-xs"><dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
      <dt className="font-semibold">Provider</dt><dd>{finding.source.providerId}{finding.source.runId && ` (run ${finding.source.runId})`}</dd>
      <dt className="font-semibold">Rule</dt><dd>{rule ?? 'none recorded'}</dd><dt className="font-semibold">Lines</dt><dd>{citedLines(finding)}</dd>
      {finding.rationale && <><dt className="font-semibold">Why</dt><dd>{finding.rationale}</dd></>}
      {!!finding.attention.reasons.length && <><dt className="font-semibold">Reasons</dt><dd>{finding.attention.reasons.join('; ')}</dd></>}
      {finding.suggestedAction && <><dt className="font-semibold">To verify</dt><dd>{finding.suggestedAction}</dd></>}
      {finding.externalUrl && <><dt className="font-semibold">Source</dt><dd><a className="text-accent underline" href={finding.externalUrl} rel="noreferrer noopener" target="_blank">Open original</a></dd></>}
    </dl>{evidenceError ? <p role="alert" className="mt-2 text-danger">{evidenceError}</p> : !evidence ? <p className="mt-2 text-muted">Loading evidence…</p> : evidence.length === 0 ? <p className="mt-2 text-muted">No evidence records were stored for this finding.</p> : <ul className="mt-2 space-y-2" aria-label={`Evidence for ${finding.title}`}>{evidence.map(record => <li key={record.id} className="rounded border border-line bg-canvas p-2"><p className="font-semibold">{record.label}</p>{evidenceValue(record) && <p>{evidenceValue(record)}</p>}<p className="text-muted">{record.kind}{record.method && ` · method ${record.method}`}{record.anchor && ` · ${citedLines({ anchor: record.anchor })}`}</p>{record.limitations.map((limitation, i) => <p key={i} className="text-warning">Limitation: {limitation}</p>)}</li>)}</ul>}</div>}
  </article>;
}
