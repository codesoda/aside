import type { SessionView } from '../api';
import PrConversation from '../insights/PrConversation';
import { Button } from '@/components/ui/button';

export type ConversationsProps = { session: SessionView; onOpenThread: (fileId: string, threadId: string) => void; onReview: () => void };
export default function Conversations({ session, onOpenThread, onReview }: ConversationsProps) {
  const fileIds = new Set(session.files.map((file) => file.id));
  const idsByPath = new Map(session.files.map((file) => [file.path, file.id]));
  return <><div className="flex items-start justify-between gap-3"><div><p className="text-xs uppercase tracking-wide text-muted-foreground">Review feedback</p><h2 className="text-xl font-semibold">All the feedback, one place.</h2><p className="text-sm text-muted-foreground">GitHub reviews and comments, alongside local review threads.</p></div><Button type="button" onClick={onReview}>Review files <span aria-hidden="true">→</span></Button></div>
    {!!(session.reviews.length || session.issueComments.length) && <PrConversation reviews={session.reviews} comments={session.issueComments} />}
    <h3 className="mt-4 font-semibold">File threads ({session.threads.length})</h3>
    {session.threads.length ? <ul className="space-y-2">{session.threads.map((thread) => {
      const fileId = thread.fileId && fileIds.has(thread.fileId) ? thread.fileId : idsByPath.get(thread.path);
      const comment = thread.comments[0];
      const preview = <><span aria-hidden="true">◎</span><span className="min-w-0 flex-1"><span className="block wrap-anywhere font-semibold">{comment?.author ?? 'Thread'} · <span className="font-mono">{thread.path}{thread.line === null ? '' : ':' + thread.line}</span></span>{comment?.body && <span className={`block text-sm text-muted-foreground ${fileId ? 'line-clamp-2' : ''}`}>{comment.body}</span>}<span className="block text-sm text-muted-foreground">{thread.resolved ? 'Resolved' : 'Open'}{!fileId && ' · file not in current diff'}</span></span><span className="rounded border px-2 text-xs">{thread.source === 'github' ? 'GitHub' : 'Local'}</span></>;
      return <li key={thread.id}>{fileId ? <Button type="button" variant="outline" className="h-auto w-full justify-start gap-2 whitespace-normal p-3 text-left" aria-label={`Open ${thread.source === 'github' ? 'GitHub' : 'local'} thread by ${comment?.author ?? 'unknown author'} on ${thread.path}${thread.line === null ? '' : ':' + thread.line}`} onClick={() => onOpenThread(fileId, thread.id)}>{preview}</Button> : <article className="flex gap-2 rounded-lg border p-3">{preview}</article>}</li>;
    })}</ul> : <p className="rounded-lg border p-3 text-sm text-muted-foreground">No file threads yet.</p>}
  </>;
}
