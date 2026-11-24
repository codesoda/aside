import { useState } from 'react';
import type { FindingItem } from '../api';
import { Button } from '../components/ui/button';
import FindingCard from './FindingCard';

export type FindingBrowserProps = {
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
};
type Bucket = 'open' | 'dismissed_local' | 'snoozed' | 'historical';
const buckets: [Bucket, string][] = [['open', 'Open'], ['dismissed_local', 'Dismissed'], ['snoozed', 'Snoozed'], ['historical', 'Historical']];
export default function FindingBrowser({ findings, loading, historicalFindings, historicalLoading, historicalLoadError, onHistorical, onHistoricalRetry, findingsWithDrafts = new Set(), pending, error, loadError = null, onRetry, onState, onPromote, onOpenDraft }: FindingBrowserProps) {
  const [bucket, setBucket] = useState<Bucket>('open');
  const [snoozeDays, setSnoozeDays] = useState(1);
  const activeLoading = bucket === 'historical' ? historicalLoading : loading;
  const activeError = bucket === 'historical' ? historicalLoadError : loadError;
  const activeFindings = bucket === 'historical' ? historicalFindings : findings;
  const visible = (activeFindings ?? []).filter(item => bucket === 'historical' ? item.label === 'historical' : item.label === 'current' && (bucket === 'open' ? !['dismissed_local', 'snoozed'].includes(item.disposition) : item.disposition === bucket));
  return <section className="workspace-panel p-3" aria-label="Browse findings">
    <h3 className="text-sm font-semibold">Browse findings</h3><p className="mt-1 text-xs text-muted">Dismissed and snoozed findings stay available here. Historical findings are read-only.</p>
    <div className="mt-2 flex flex-wrap gap-2" role="group" aria-label="Finding status">{buckets.map(([value, label]) => <Button key={value} type="button" variant="outline" aria-pressed={bucket === value} onClick={() => { setBucket(value); if (value === 'historical') onHistorical(); }}>{label}</Button>)}</div>
    {bucket === 'historical' && <p className="mt-2 text-xs text-muted">Past snapshots only. These findings cannot be changed or promoted.</p>}
    {error && <p role="alert" className="mt-2 text-danger">{error}</p>}
    {activeLoading ? <p className="mt-2 text-sm text-muted">Loading findings…</p> : activeError ? <div className="mt-2" role="alert"><p>Could not load findings: {activeError}</p><Button type="button" variant="outline" onClick={bucket === 'historical' ? onHistoricalRetry : onRetry}>Retry finding load</Button></div> : activeFindings === null && bucket === 'historical' ? <Button type="button" variant="outline" className="mt-2" onClick={onHistorical}>Load historical findings</Button> : activeFindings === null ? <p className="mt-2 text-sm text-muted">Waiting for findings from the current snapshot…</p> : !visible.length ? <p className="mt-2 text-sm text-muted">No {bucket.replace('_local', '').replace('_', ' ')} findings.</p> : <ul className="mt-3 space-y-3">{visible.map(item => <li key={item.finding.id}><FindingCard item={item} />{item.label === 'current' && <div className="mb-3 flex flex-wrap items-center gap-2">
      {bucket !== 'open' && <Button type="button" variant="outline" disabled={pending !== null} onClick={() => onState(item, 'untouched')}>Restore {item.finding.title}</Button>}
      {bucket !== 'dismissed_local' && <Button type="button" variant="outline" disabled={pending !== null} onClick={() => onState(item, 'dismissed_local')}>Dismiss {item.finding.title}</Button>}
      {bucket !== 'snoozed' ? <><label className="text-xs">Snooze for <select className="workspace-input" value={snoozeDays} onChange={event => setSnoozeDays(Number(event.target.value))} aria-label={`Snooze duration for ${item.finding.title}`}><option value={1}>1 day</option><option value={7}>7 days</option><option value={30}>30 days</option></select></label><Button type="button" variant="outline" disabled={pending !== null} onClick={() => onState(item, 'snoozed', new Date(Date.now() + snoozeDays * 86400000).toISOString())}>Snooze {item.finding.title}</Button></> : item.userState?.snoozedUntil && <span className="text-xs text-muted">Until {new Date(item.userState.snoozedUntil).toLocaleString()}</span>}
      {findingsWithDrafts.has(item.finding.id) ? <Button type="button" variant="outline" onClick={() => onOpenDraft(item)}>Open existing draft</Button> : item.canPromote && <Button type="button" variant="outline" disabled={pending !== null} onClick={() => onPromote(item)}>Promote to draft</Button>}
    </div>}</li>)}</ul>}
  </section>;
}
