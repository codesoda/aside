import { useEffect, useRef, useState } from 'react';
import { ApiError, confirmPublication, fetchPublicationStatus, fetchSession, finishReview, previewPublication, reconcilePublication, refreshReview, type ActivePublication, type PublishOutcome, type PublishPreview, type SessionView, type Verdict } from '../api';
import { Button } from '../components/ui/button';
import { Textarea } from '../components/ui/textarea';

type Props = { session: SessionView; onSession: (view: SessionView) => boolean | void };

export default function FinishReview({ session, onSession }: Props) {
  const [verdict, setVerdict] = useState<Verdict>(() => session.review.verdict ?? 'COMMENT');
  const [summary, setSummary] = useState(() => session.review.summary);
  const [preview, setPreview] = useState<PublishPreview | null>(null);
  const [outcome, setOutcome] = useState<PublishOutcome | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [stale, setStale] = useState(false);
  const [active, setActive] = useState<ActivePublication | null>(null);
  const [checked, setChecked] = useState(false);
  const [transportUnknown, setTransportUnknown] = useState(false);
  const [choice, setChoice] = useState<'posted' | 'not_posted' | ''>('');
  const [choiceAttemptId, setChoiceAttemptId] = useState<string | null>(null);
  const [statusError, setStatusError] = useState('');
  const lookupVersion = useRef(0);
  const activeRef = useRef<ActivePublication | null>(null);
  const review = session.review;
  const locked = !checked || !!active || !!review.unresolvedPublication || transportUnknown;
  const planStale = stale || (preview !== null && (preview.plan.selectionRevision !== review.selectionRevision || preview.plan.headSha !== session.pr.headSha || preview.plan.baseSha !== session.pr.baseSha || preview.plan.verdict !== review.verdict || preview.plan.summary !== review.summary));

  async function lookup() {
    const version = ++lookupVersion.current;
    try {
      const response = await fetchPublicationStatus();
      if (version !== lookupVersion.current) return;
      if (response.active?.attemptId !== activeRef.current?.attemptId) { setChoice(''); setChoiceAttemptId(null); }
      activeRef.current = response.active;
      setActive(response.active); setChecked(true); setStatusError('');
      return true;
    } catch {
      if (version !== lookupVersion.current) return;
      setChecked(false);
      setStatusError('Could not check publication status. Do not submit until the status is available.');
      return false;
    }
  }
  useEffect(() => {
    if (session.demo) return;
    void lookup();
    return () => { ++lookupVersion.current; };
  }, [session.demo, review.unresolvedPublication?.attemptId]);

  async function reconcile() {
    if (busyRef.current || !active || !choice || choiceAttemptId !== active.attemptId) return;
    busyRef.current = true; setBusy(true); setError('');
    try {
      if (onSession(await fetchSession()) === false) return;
      await reconcilePublication({ attemptId: active.attemptId, outcome: choice, confirm: true });
      onSession(await fetchSession());
      setTransportUnknown(false);
      const confirmed = await lookup();
      const response = activeRef.current;
      if (!confirmed || response) setError('Reconciliation was sent, but publication status is not confirmed. Check again before submitting.');
      else { setPreview(null); setOutcome(null); setChoice(''); setStale(true); }
    } catch (failure) { setError(`${failure instanceof Error ? failure.message : 'Reconciliation failed.'} Check GitHub; do not resubmit.`); }
    finally { busyRef.current = false; setBusy(false); }
  }
  async function showPreview() {
    if (busyRef.current || locked || session.demo) return;
    busyRef.current = true; setBusy(true); setError(''); setPreview(null); setOutcome(null); setStale(false);
    const body = summary.trim();
    try {
      if (onSession(await fetchSession()) === false) return;
      await finishReview(verdict, body);
      if (onSession(await fetchSession()) === false) return;
      setPreview(await previewPublication(verdict, body));
    } catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { busyRef.current = false; setBusy(false); }
  }
  async function confirm() {
    if (busyRef.current || !preview || preview.blockers.length || planStale || session.demo || locked) return;
    busyRef.current = true; setBusy(true); setError('');
    try {
      if (onSession(await fetchSession()) === false) return;
      const result = await confirmPublication(preview.plan.planId, preview.plan.verdict, preview.plan.summary);
      setOutcome(result.outcome); setPreview(null);
      onSession(await fetchSession());
      await lookup();
      if (result.outcome.status !== 'succeeded' && result.outcome.status !== 'rejected') setTransportUnknown(true);
    } catch (failure) {
      if (failure instanceof ApiError && ['plan_changed', 'pr_changed', 'publish_blocked', 'publish_conflict'].includes(failure.code)) {
        setStale(true);
        setError(failure.code === 'pr_changed' ? 'The pull request changed. Refresh it and make a new preview before publishing.' : failure.code === 'plan_changed' ? 'Drafts or selection changed. Make a new preview before publishing.' : 'Publication is blocked. Check GitHub for an earlier attempt; do not retry without checking.');
      } else { setStale(true); setTransportUnknown(true); setError('The publication outcome is uncertain. Check GitHub before taking any further action.'); }
      try { onSession(await fetchSession()); } catch { /* Keep uncertainty visible. */ }
      await lookup();
    } finally { busyRef.current = false; setBusy(false); }
  }
  async function refresh() {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setPreview(null); setStale(true); setError('');
    try { onSession((await refreshReview()).session); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { busyRef.current = false; setBusy(false); }
  }
  return <><div className="workspace-view-heading"><div><p className="workspace-eyebrow">Finish review</p><h2>Choose what goes to GitHub.</h2><p className="workspace-subtitle">Only a human can publish. Omitted drafts stay local. A partial or uncertain attempt is never retried automatically.</p></div></div>
    <div className="workspace-panel p-4 space-y-4">
      {review.unresolvedPublication && <p role="alert" className="text-danger">Unresolved publication ({review.unresolvedPublication.status}, attempt {review.unresolvedPublication.attemptId}). Check GitHub before deciding what happened. Unsent drafts remain local.</p>}
      {statusError && <><p role="alert" className="text-danger">{statusError}</p><Button onClick={() => void lookup()} disabled={busy}>Check publication status</Button></>}
      {locked && !statusError && !session.demo && <section aria-label="Resolve uncertain publication" className="space-y-2"><h3 className="font-semibold">Check GitHub before continuing</h3><p>The review may already be posted. Do not retry. Verify the review and comments on GitHub before recording the outcome.</p>
        {active ? <><p>Attempt: {active.attemptId}</p>{active.affectedDraftIds.length > 0 && <p>Affected draft IDs: {active.affectedDraftIds.join(', ')}</p>}<label>What did you verify on GitHub?<select aria-label="Verified publication outcome" className="workspace-input block" disabled={busy} value={choice} onChange={(event) => { setChoice(event.target.value as typeof choice); setChoiceAttemptId(active.attemptId); }}><option value="">Select only after checking GitHub</option><option value="posted">Review posted</option><option value="not_posted">Review did not post</option></select></label><Button disabled={busy || !choice || choiceAttemptId !== active.attemptId} onClick={() => void reconcile()}>Confirm checked outcome</Button></> : <p>No authoritative active attempt ID is available. Do not guess an ID or resubmit.</p>}
      </section>}
      <label className="block">Review verdict<select className="workspace-input block" value={verdict} disabled={busy} onChange={(event) => { setVerdict(event.target.value as Verdict); if (preview) setStale(true); }}><option value="COMMENT">Comment</option><option value="APPROVE">Approve</option><option value="REQUEST_CHANGES">Request changes</option></select></label>
      <label className="block">Review summary<Textarea className="workspace-input block w-full" rows={4} value={summary} disabled={busy} onChange={(event) => { setSummary(event.target.value); if (preview) setStale(true); }} /></label>
      <Button onClick={() => void showPreview()} disabled={busy || session.demo || locked}>Finish and preview</Button>
      {session.demo && <p>Demo is local only. Publication previews and publishing are unavailable.</p>}
      {error && <p role="alert" className="text-danger">{error}</p>}
      {planStale && <><p role="status">This preview is stale. Make a new preview before publishing.</p><Button onClick={() => void refresh()} disabled={busy}>Refresh pull request</Button></>}
      {preview && <section aria-label="Publication preview" className="space-y-3"><h3 className="font-semibold">Exact publication preview</h3><p>Verdict: {preview.plan.verdict} · Summary:</p><p className="whitespace-pre-wrap">{preview.plan.summary || '(empty)'}</p>
        {!preview.plan.inline.length && !preview.plan.replies.length && <p>No prepared comments selected. This review sends the verdict and summary only.</p>}
        <h4 className="font-semibold">Included destinations and bodies</h4><ul className="space-y-2">{preview.plan.inline.map((item) => <li key={item.commentId} className="border border-line p-2">{item.path} · {item.side} {item.startLine && item.startLine !== item.line ? `lines ${item.startLine}–${item.line}` : `line ${item.line}`} · {item.authorKind}<p className="whitespace-pre-wrap break-words">{item.body}</p></li>)}{preview.plan.replies.map((item) => <li key={item.commentId} className="border border-line p-2">Reply in {item.path} · {item.side} line {item.line ?? 'unknown'} · parent {item.parentCommentId} · {item.authorKind}<p className="whitespace-pre-wrap break-words">{item.body}</p></li>)}</ul>
        <h4 className="font-semibold">Kept local / omitted drafts</h4>{!preview.plan.omitted.length && <p>None.</p>}<ul>{preview.plan.omitted.map((item) => <li key={item.commentId}>{item.kind} {item.commentId} · {item.selected ? 'selected' : 'not selected'} · {item.code}: {item.reason}</li>)}</ul>
        {!!preview.blockers.length && <><h4 className="font-semibold">Publication blockers</h4><ul>{preview.blockers.map((blocker, index) => <li role="alert" key={index}>{blocker.code}: {blocker.message}</li>)}</ul></>}
        <Button className="primary-button" onClick={() => void confirm()} disabled={busy || planStale || preview.blockers.length > 0 || session.demo || locked}>Publish to GitHub</Button></section>}
      {outcome && <section aria-label="Publication result" className="space-y-2"><h3 className="font-semibold">Publication {outcome.status}</h3><p>Published: {outcome.publishedCommentIds.join(', ') || 'none'}</p><p>Remaining unsent: {outcome.remainingCommentIds.join(', ') || 'none'}</p><p>Omitted: {outcome.omittedCommentIds.join(', ') || 'none'}</p>{outcome.status === 'partial' && <p role="alert">Check GitHub. Aside will not retry this attempt. Unsent drafts remain local.</p>}{outcome.error && <p role="alert">{outcome.error}</p>}</section>}
    </div></>;
}
