import type { PullRequest } from './generated/PullRequest';

/** Keep all available review identity; let the browser truncate it visually. */
export function reviewPageTitle(pr: Partial<PullRequest> | null | undefined): string {
  const number = pr?.number != null && pr.number > 0 ? `#${pr.number}` : '';
  const review = [number, pr?.title?.trim()].filter(Boolean).join(': ') || 'Review';
  const repository = [pr?.owner?.trim(), pr?.repo?.trim()].filter(Boolean).join('/');
  return [review, repository, 'Aside'].filter(Boolean).join(' · ');
}
