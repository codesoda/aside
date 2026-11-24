import { useRef, useState } from 'react';
import type { ConversationThread, FindingItem } from '../api';
import { deleteReviewComment, patchThread, replyToThread, retryAgent, saveReviewComment } from '../api';
import { Button } from '@/components/ui/button';
import { Textarea } from '@/components/ui/textarea';

export type ConversationDraft = { reply: string; outgoing: string; editing: boolean };
export type ConversationPanelProps = {
  thread: ConversationThread;
  drafts: Record<string, ConversationDraft>;
  finding?: FindingItem;
  mobile?: boolean;
  onDraftChange?: () => void;
  pendingActions?: Set<string>;
  agentConnected?: boolean;
  onRefresh: () => Promise<void>;
  onError: (message: string) => void;
  onClose: () => void;
};

export default function ConversationPanel({ thread, drafts, finding, mobile = false, agentConnected = true, onDraftChange, pendingActions, onRefresh, onError, onClose }: ConversationPanelProps) {
  const localPending = useRef(new Set<string>());
  const pending = pendingActions ?? localPending.current;
  const currentId = useRef(thread.id);
  currentId.current = thread.id;
  const [, render] = useState(0);
  const refreshLocal = () => { if (onDraftChange) onDraftChange(); else render((value) => value + 1); };
  const busy = pending.has(thread.id);
  const draft = drafts[thread.id];
  const reply = draft?.reply ?? '';
  const outgoing = draft?.outgoing ?? thread.reviewComment?.body ?? '';
  const editing = draft?.editing ?? false;
  const updateDraft = (change: Partial<ConversationDraft>) => {
    drafts[thread.id] = { reply, outgoing, editing, ...change };
    refreshLocal();
  };
  const completeDraft = (id: string, field: 'reply' | 'outgoing', submitted: string, change: Partial<ConversationDraft>) => {
    const current = drafts[id];
    if (current?.[field] === submitted) { drafts[id] = { ...current, ...change }; refreshLocal(); }
  };
  const action = async (id: string, operation: () => Promise<void>) => {
    if (pending.has(id)) return;
    pending.add(id); refreshLocal();
    try { await operation(); await onRefresh(); }
    catch (error) { if (currentId.current === id) onError(error instanceof Error ? error.message : String(error)); }
    finally { pending.delete(id); refreshLocal(); }
  };
  const waiting = thread.agent?.state === 'waiting' || thread.agent?.state === 'responding';
  const sendReply = () => {
    const id = thread.id, submitted = reply, body = submitted.trim();
    if (!body || waiting || pending.has(id)) return;
    void action(id, async () => { await replyToThread(id, body); completeDraft(id, 'reply', submitted, { reply: '' }); });
  };
  const copyAgent = (body: string, commentId: string) => {
    if (outgoing.trim() && outgoing !== body && !confirm('Replace the current outgoing review comment with this agent reply?')) return;
    const id = thread.id;
    updateDraft({ outgoing: body, editing: true });
    void action(id, () => saveReviewComment(id, body, commentId));
  };
  return <article id={(mobile ? 'thread-mobile-' : 'thread-') + thread.id} tabIndex={-1} className="review-thread is-selected" aria-label="Selected conversation">
    <div className="flex items-start justify-between gap-2"><div>
      <p className="text-xs text-muted-foreground">{thread.source === 'github' ? 'GitHub feedback' : 'Private conversation'} · {thread.subject === 'file' || thread.line === null ? 'File' : `${thread.side} ${thread.startLine && thread.startLine !== thread.line ? `lines ${thread.startLine}–${thread.line}` : `line ${thread.line}`}`} · {thread.anchorState}</p>
      {finding && <><h3 className="mt-2 font-semibold">{finding.finding.title}</h3><p className="mt-1 text-sm">{finding.finding.rationale}</p></>}
    </div><Button type="button" variant="ghost" size="icon-sm" aria-label="Close conversation" onClick={onClose}>×</Button></div>
    {thread.comments.map((comment) => <div key={comment.id} className="mt-3 border-t pt-2"><p className="text-xs font-semibold">{comment.author} · {comment.publishState}</p><p className="mt-1 whitespace-pre-wrap break-words">{comment.body}</p>
      {comment.authorKind === 'agent' && comment.publishState === 'private' && <Button type="button" variant="outline" size="sm" className="mt-2" disabled={busy} onClick={() => copyAgent(comment.body, comment.id)} title="Copy verbatim to outgoing review comment">+ Add to review</Button>}</div>)}
    {waiting ? <p role="status" className="mt-3 text-sm">Agent {thread.agent?.state}<span className="agent-dots" aria-hidden="true">…</span></p> : thread.agent?.state === 'failed' ? <p role="status" className="mt-3 text-sm">Agent failed{thread.agent.error ? `: ${thread.agent.error}` : ''}. {thread.agent.requestCommentId && <Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => { const id = thread.id, requestId = thread.agent!.requestCommentId!; void action(id, () => retryAgent(id, requestId)); }}>Retry</Button>}</p> : (thread.agent?.state === 'no_agent' || !agentConnected) && <p role="status" className="mt-3 text-sm text-muted-foreground">No agent connected.</p>}
    <form className="mt-3" onSubmit={(event) => { event.preventDefault(); sendReply(); }}><label htmlFor={'reply-' + (mobile ? 'mobile-' : 'desktop-') + thread.id} className="text-xs font-semibold">Private reply</label>
      <Textarea id={'reply-' + (mobile ? 'mobile-' : 'desktop-') + thread.id} rows={2} value={reply} onChange={(event) => updateDraft({ reply: event.currentTarget.value })} className="mt-1 w-full" placeholder="Discuss privately" onKeyDown={(event) => { if (event.key === 'Enter' && (event.metaKey || event.ctrlKey) && !event.nativeEvent.isComposing && !event.repeat) { event.preventDefault(); sendReply(); } }} />
      {waiting && <p className="text-xs text-muted-foreground">Wait for the current agent response before sending another reply.</p>}
      <Button type="submit" size="sm" disabled={busy || !reply.trim() || waiting} className="mt-1">Send private reply</Button></form>
    {thread.source === 'local' && <div className="mt-3 flex gap-2"><Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => { const id = thread.id, resolved = !thread.resolved; void action(id, () => patchThread(id, { resolved })); }}>{thread.resolved ? 'Reopen locally' : 'Mark handled locally'}</Button></div>}
    <section className="mt-4 border-t pt-3" aria-label="Outgoing review comment"><div className="flex items-center justify-between gap-2"><h4 className="font-semibold">Outgoing review comment</h4>
      {thread.reviewComment && <Button type="button" variant="outline" size="sm" disabled={busy} aria-label="Clear outgoing review comment" onClick={() => { const id = thread.id, submitted = outgoing; void action(id, async () => { await deleteReviewComment(id); completeDraft(id, 'outgoing', submitted, { outgoing: '', editing: false }); }); }}>× Clear</Button>}</div>
      <p className="text-xs text-muted-foreground">{thread.source === 'github' ? 'This draft replies to the GitHub thread when you submit the review. It does not resolve the upstream thread.' : 'This draft is submitted as a review comment at its file anchor.'} Private replies are never included.</p>
      {thread.reviewComment?.target && <p className="text-xs text-muted-foreground">Target: {thread.reviewComment.target.kind === 'reply' ? 'GitHub thread reply' : thread.reviewComment.target.kind === 'file_fallback' ? 'file comment' : 'inline comment'} · {thread.reviewComment.target.path}{thread.reviewComment.target.line === null ? '' : `:${thread.reviewComment.target.line}`}</p>}
      {thread.reviewComment?.blocker && <p role="status" className="text-xs text-muted-foreground">Cannot submit yet: {thread.reviewComment.blocker}</p>}
      {editing || !thread.reviewComment ? <><Textarea aria-label="Review comment draft" rows={3} value={outgoing} onChange={(event) => { const value = event.currentTarget.value; updateDraft({ outgoing: value }); if (!value && thread.reviewComment && !pending.has(thread.id)) { const id = thread.id; void action(id, async () => { await deleteReviewComment(id); completeDraft(id, 'outgoing', value, { outgoing: '', editing: true }); }); } }} className="mt-2 w-full" placeholder="Write an outgoing review comment" />
        <Button type="button" size="sm" className="mt-1" disabled={busy || !outgoing.trim()} onClick={() => { const id = thread.id, submitted = outgoing, body = submitted.trim(); void action(id, async () => { await saveReviewComment(id, body); completeDraft(id, 'outgoing', submitted, { outgoing: body, editing: false }); }); }}>Save review comment</Button></> : <Button type="button" variant="outline" className="mt-2 h-auto w-full justify-start whitespace-pre-wrap text-left" onClick={() => updateDraft({ outgoing: thread.reviewComment?.body ?? '', editing: true })} aria-label="Edit outgoing review comment">{thread.reviewComment.body}</Button>}
    </section>
  </article>;
}
