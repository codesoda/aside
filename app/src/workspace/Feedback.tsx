import { useState } from 'react';
import type { FeedbackThread, NativeFeedbackRecord } from '../api';
import type { Page } from '../generated/Page';
import { anchorText, commentLink, label, link, nativeText, searchableRecord, searchableThread, sourceKind, threadLink, unresolvedByPath, value, nativeComment as nativeCommentFor, resolutionKnown } from '../feedback';
import { Button } from '../components/ui/button';
import { Input } from '../components/ui/input';

type Props = { threads: FeedbackThread[]; records: NativeFeedbackRecord[]; threadPage: Page<FeedbackThread> | null; recordPage: Page<NativeFeedbackRecord> | null; loading: boolean; errors: string[]; ignoredPaths: Set<string>; availablePaths: Set<string>; headSha: string; onOpenFile: (path: string) => void };

export default function Feedback({ threads, records, threadPage, recordPage, loading, errors, ignoredPaths, availablePaths, headSha, onOpenFile }: Props) {
  const [query, setQuery] = useState('');
  const [revealIgnored, setRevealIgnored] = useState(false);
  const counts = unresolvedByPath(threads);
  const ignoredUnresolved = [...counts].filter(([path]) => ignoredPaths.has(path)).reduce((sum, [, count]) => sum + count, 0);
  const ignoredUnknown = threads.filter((thread) => ignoredPaths.has(thread.path) && !resolutionKnown(thread)).length;
  const matchedThreads = threads.filter((thread) => searchableThread(thread).includes(query.trim().toLowerCase()) && (!ignoredPaths.has(thread.path) || revealIgnored));
  const matchedRecords = records.filter((record) => searchableRecord(record).includes(query.trim().toLowerCase()));
  const kinds = [...new Set(records.map(sourceKind))];
  return <section className="workspace-panel p-4" aria-label="Native GitHub feedback">
    <h2 className="text-lg font-semibold">GitHub feedback · read-only</h2>
    <p className="text-sm text-muted">Imported feedback is separate from Aside findings. Search covers all loaded pages, including replies. Search cannot cover GitHub data that was not imported.</p>
    {loading && <p role="status">Loading native feedback pages… Search results are incomplete.</p>}
    {errors.map((error, index) => <p key={index} role="alert" className="text-danger">Feedback load failed: {error}. Search results are incomplete.</p>)}
    <label className="mt-3 block text-sm font-semibold" htmlFor="feedback-search">Search imported feedback</label>
    <Input id="feedback-search" type="search" value={query} onChange={(event) => setQuery(event.target.value)} className="workspace-input mt-1 w-full" />
    <p className="mt-2 text-sm">{ignoredUnresolved} confirmed unresolved {ignoredUnresolved === 1 ? 'thread' : 'threads'} on ignored files · {ignoredUnknown} with resolution unknown{threadPage?.coverage.complete === false || !threadPage ? ' (known counts; coverage incomplete)' : ''}.</p>
    <Button className="mt-2" aria-pressed={revealIgnored} onClick={() => setRevealIgnored(!revealIgnored)}>{revealIgnored ? 'Hide ignored feedback' : 'Reveal ignored feedback'}</Button>
    <p className="mt-2 text-xs text-muted">Reveal changes this view only. It does not include files in review or mark them viewed.</p>
    <div className="mt-4" aria-label="Feedback source coverage"><h3 className="font-semibold">Source coverage</h3>
      {!threadPage && !recordPage && <p>Coverage unavailable.</p>}
      {[threadPage, recordPage].map((page, index) => page && <div key={index}>{page.coverage.sources.map((source) => <p key={source.sourceId} className="text-sm">{source.sourceId}: {source.state} · {source.completeness} · {source.itemKind} {source.items.state === 'known' ? source.items.value : 'count unknown'}{source.runId ? ` · run ${source.runId}` : ''}{source.limitations.length ? ` · ${source.limitations.join('; ')}` : ''}</p>)}{!page.coverage.complete && <p className="text-warning text-sm">This source is incomplete or unavailable; empty results are not all clear.</p>}</div>)}
    </div>
    <section className="mt-5" aria-label="Imported review threads"><h3 className="font-semibold">Review threads and replies ({matchedThreads.length})</h3>
      {!loading && threadPage?.coverage.complete && threads.length === 0 && <p>No imported review threads (complete source).</p>}
      {matchedThreads.map((thread) => {
        const native = thread.github;
        const commit = native?.comments.find((comment) => comment.commitOid)?.commitOid;
        const href = threadLink(thread);
        return <article className="review-thread mt-2" data-feedback-thread={thread.id} key={thread.id}>
          <p className="text-xs">GitHub thread · {resolutionKnown(thread) ? native?.isResolved ? 'Resolved' : 'Unresolved' : native?.restOnly ? 'Resolution unknown (REST-only)' : 'Resolution unknown'} · anchor {thread.anchorState}{thread.anchorReason ? ` · ${thread.anchorReason}` : ''} · created {thread.createdAt || 'unknown'}</p>
          {native && <p className="text-xs">Native anchor: {native.isOutdated ? 'outdated' : 'not marked outdated'} · {native.subjectType ?? 'subject unknown'} · original line {native.originalLine ?? 'unknown'}{native.resolvedBy ? ` · resolved by ${native.resolvedBy}` : ''}</p>}
          <p className="text-sm">Anchor: {thread.path || 'Unknown'} · {thread.side} {thread.startLine && thread.line && thread.startLine !== thread.line ? `lines ${thread.startLine}–${thread.line}` : `line ${thread.line ?? 'unknown'}`} · {commit && headSha ? commit === headSha ? 'native commit matches current head (not necessarily refreshed)' : 'native commit differs from current head' : 'freshness unknown'}{commit ? ` · native commit ${commit}` : ''}{ignoredPaths.has(thread.path) ? ' · ignored file' : ''}</p>
          {href ? <a href={href} target="_blank" rel="noopener noreferrer" className="text-accent underline">Open thread on GitHub</a> : <span className="text-xs text-muted">Thread source link unavailable</span>}
          {availablePaths.has(thread.path) ? <Button className="ml-2" onClick={() => onOpenFile(thread.path)}>Open {thread.path} in review</Button> : <p className="text-xs text-muted">No matching current file; this anchor cannot be opened in the review.</p>}
          {thread.comments.map((comment) => { const nativeComment = nativeCommentFor(thread, comment); const commentHref = commentLink(thread, comment); return <div key={comment.id} className="mt-2 border-t border-line pt-2"><p className="text-xs">{comment.author || 'Unknown author'}{nativeComment?.authorType ? ` (${nativeComment.authorType})` : ''} · {comment.publishState} · {comment.createdAt || 'date unknown'} · updated {comment.updatedAt || 'unknown'}</p>{nativeComment && <p className="text-xs">Native comment anchor: {nativeComment.outdated ? 'outdated' : 'not marked outdated'} · line {nativeComment.line ?? 'unknown'} · original line {nativeComment.originalLine ?? 'unknown'}</p>}<p className="whitespace-pre-wrap break-words">{comment.body}</p>{commentHref ? <a href={commentHref} target="_blank" rel="noopener noreferrer" className="text-accent underline">Open comment on GitHub</a> : <span className="text-xs text-muted">Comment source link unavailable</span>}</div>; })}
        </article>;
      })}
    </section>
    {kinds.map((kind) => <section key={kind} className="mt-5" aria-label={kind}><h3 className="font-semibold">{kind} ({matchedRecords.filter((record) => sourceKind(record) === kind).length})</h3>
      {matchedRecords.filter((record) => sourceKind(record) === kind).map((record, index) => { const raw = value(record); const href = link(record); return <article className="review-thread mt-2" key={index}>
        <p className="text-xs">{kind} · {nativeText(record, 'author', 'appSlug', 'producer', 'context', 'name')} · {nativeText(record, 'state', 'status', 'conclusion', 'annotationLevel')}</p>
        <p className="text-sm">Anchor: {anchorText(record)} · freshness: {record.currentHead === true ? 'current head (not necessarily refreshed)' : record.currentHead === false ? 'historical head' : 'unknown'} · head {nativeText(record, 'headSha', 'sha', 'commitSha')} · {nativeText(record, 'updatedAt', 'completedAt', 'createdAt')}</p>
        {!!(raw.annotationCoverage || record.annotationCoverage) && <p className="text-sm">Annotation coverage: {JSON.stringify(raw.annotationCoverage ?? record.annotationCoverage)}</p>}
        {!!(raw.checkRunId || raw.githubCheckRunId || raw.runId) && <p className="text-xs">Run: {label(String(raw.checkRunId ?? raw.githubCheckRunId ?? raw.runId))}{raw.annotationCoverage || record.annotationCoverage ? '' : ' · annotation coverage unknown'}</p>}
        <p className="whitespace-pre-wrap break-words">{nativeText(record, 'body', 'message', 'summary', 'title')}</p>
        {href ? <a href={href} target="_blank" rel="noopener noreferrer" className="text-accent underline">Open source</a> : <span className="text-xs text-muted">Source link unavailable</span>}
        <details><summary>Native metadata</summary><pre className="overflow-auto text-xs">{JSON.stringify(record, null, 2)}</pre></details>
      </article>; })}
    </section>)}
    {!loading && records.length === 0 && recordPage?.coverage.complete && <p>No other imported feedback (complete sources).</p>}
  </section>;
}
