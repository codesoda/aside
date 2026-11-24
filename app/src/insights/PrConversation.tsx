import type { ImportedReview } from '../generated/ImportedReview';
import type { IssueComment } from '../generated/IssueComment';

export type PrConversationProps = { reviews: ImportedReview[]; comments: IssueComment[] };
export default function PrConversation({ reviews, comments }: PrConversationProps) {
  if (!reviews.length && !comments.length) return null;
  return <details open className="rounded-lg border p-3 text-sm"><summary className="cursor-pointer text-primary focus-visible:outline-2 focus-visible:outline-ring">PR conversation on GitHub ({reviews.length} {reviews.length === 1 ? 'review' : 'reviews'}, {comments.length} {comments.length === 1 ? 'comment' : 'comments'})</summary>
    <ul className="mt-2 space-y-2">{reviews.map((review, i) => <li key={`review-${i}`}><article className="rounded-lg border bg-muted/30 p-3 wrap-anywhere" aria-label={`External feedback: review by ${review.author}, ${review.state.toLowerCase()}`}><p className="text-xs"><span className="font-semibold"><span aria-hidden="true">❝</span> External feedback · GitHub review</span> · {review.author} · {review.state.toLowerCase().replaceAll('_', ' ')}{review.submittedAt && ` · ${review.submittedAt}`}</p>{review.body && <p className="mt-1 whitespace-pre-wrap break-words">{review.body}</p>}</article></li>)}
      {comments.map((comment, i) => <li key={`comment-${i}`}><article className="rounded-lg border bg-muted/30 p-3 wrap-anywhere" aria-label={`External feedback: comment by ${comment.author}`}><p className="text-xs"><span className="font-semibold"><span aria-hidden="true">❝</span> External feedback · GitHub comment</span> · {comment.author}{comment.createdAt && ` · ${comment.createdAt}`}</p><p className="mt-1 whitespace-pre-wrap break-words">{comment.body}</p></article></li>)}</ul>
  </details>;
}
