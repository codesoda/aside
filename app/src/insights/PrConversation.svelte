<script lang="ts">
  import type { ImportedReview } from '../generated/ImportedReview';
  import type { IssueComment } from '../generated/IssueComment';

  let { reviews, comments }: { reviews: ImportedReview[]; comments: IssueComment[] } = $props();
</script>

{#if reviews.length || comments.length}
  <!-- PR-level GitHub feedback keeps its author and is never given a line. -->
  <details open class="workspace-panel p-3 text-sm">
    <summary class="cursor-pointer text-accent focus-visible:outline-2 focus-visible:outline-accent">
      PR conversation on GitHub ({reviews.length} {reviews.length === 1 ? 'review' : 'reviews'}, {comments.length} {comments.length === 1 ? 'comment' : 'comments'})
    </summary>
    <ul class="mt-2 space-y-2">
      {#each reviews as review, i (i)}
        <li><article class="rounded-lg border border-line bg-sidebar p-3 wrap-anywhere" aria-label={`External feedback: review by ${review.author}, ${review.state.toLowerCase()}`}>
          <p class="text-xs"><span class="font-semibold"><span aria-hidden="true">❝</span> External feedback · GitHub review</span> · {review.author} · {review.state.toLowerCase().replaceAll('_', ' ')}{#if review.submittedAt} · {review.submittedAt}{/if}</p>
          {#if review.body}<p class="mt-1 whitespace-pre-wrap break-words">{review.body}</p>{/if}
        </article></li>
      {/each}
      {#each comments as comment, i (i)}
        <li><article class="rounded-lg border border-line bg-sidebar p-3 wrap-anywhere" aria-label={`External feedback: comment by ${comment.author}`}>
          <p class="text-xs"><span class="font-semibold"><span aria-hidden="true">❝</span> External feedback · GitHub comment</span> · {comment.author}{#if comment.createdAt} · {comment.createdAt}{/if}</p>
          <p class="mt-1 whitespace-pre-wrap break-words">{comment.body}</p>
        </article></li>
      {/each}
    </ul>
  </details>
{/if}
