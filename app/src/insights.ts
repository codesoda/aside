// Pure presentation logic for review insights (#38). No I/O, no review
// mutation: nothing here can mark a file viewed or reviewed.
import type { AnalysisSummary } from './generated/AnalysisSummary';
import type { AssertionKind } from './generated/AssertionKind';
import type { Evidence } from './generated/Evidence';
import type { FindingItem } from './generated/FindingItem';
import type { QueueItem } from './generated/QueueItem';
import type { QueueVerdict } from './generated/QueueVerdict';
import type { Reason } from './generated/Reason';
import type { ReasonCode } from './generated/ReasonCode';
import type { ReviewFinding } from './generated/ReviewFinding';
import type { ReviewThread } from './generated/ReviewThread';
import type { SourceCoverage } from './generated/SourceCoverage';
import type { SourceState } from './generated/SourceState';

/** The three visible classes (analyze.md §3.2). */
export type InsightClass = 'measured' | 'suggestion' | 'external';

export type AssertionPresentation = {
  cls: InsightClass;
  /** Visible text label; also used in the accessible name. */
  label: string;
  /** Decorative glyph; never the only carrier of meaning. */
  glyph: string;
};

/**
 * Classifies by `AssertionKind`, never by text. Stored findings are served
 * verbatim, so an unrecognised kind arrives as its original string and falls
 * back to the least assertive class: it is never shown as measured.
 */
export function presentAssertion(kind: AssertionKind | string, validation?: string): AssertionPresentation {
  switch (kind) {
    case 'measured_fact':
      return { cls: 'measured', label: 'Measured fact', glyph: '▣' };
    case 'confirmed_test_failure':
      return { cls: 'measured', label: 'Confirmed test failure', glyph: '▣' };
    case 'source_status':
      return { cls: 'measured', label: 'Source status', glyph: '▣' };
    case 'external_feedback':
      return { cls: 'external', label: 'External feedback', glyph: '❝' };
    case 'defect_hypothesis':
      return {
        cls: 'suggestion',
        label: validation === 'supported' ? 'Validated hypothesis' : 'Unverified hypothesis',
        glyph: '?',
      };
    case 'attention_hint':
      return { cls: 'suggestion', label: 'Suggestion · attention hint', glyph: '?' };
    case 'informational':
      return { cls: 'suggestion', label: 'Suggestion · informational', glyph: '?' };
    default:
      return { cls: 'suggestion', label: `Suggestion · unrecognised kind "${kind}"`, glyph: '?' };
  }
}

const SOURCE_STATE_TEXT: Record<SourceState, string> = {
  pending: 'running',
  clean: 'complete',
  has_results: 'complete',
  incomplete: 'partial',
  failed: 'failed',
  not_run: 'not run',
};

export function sourceStateText(state: SourceState | string): string {
  return SOURCE_STATE_TEXT[state as SourceState] ?? `unknown state "${state}"`;
}

/** Whether a source is complete over its scope. Unknown states are not. */
export function sourceComplete(source: SourceCoverage): boolean {
  return source.state === 'clean' || source.state === 'has_results';
}

export type CoverageTone = 'complete' | 'pending' | 'incomplete';

export function coverageTone(summary: Pick<AnalysisSummary, 'overall'>): CoverageTone {
  return summary.overall === 'complete' ? 'complete' : summary.overall === 'pending' ? 'pending' : 'incomplete';
}

/** Short status line, e.g. "Intelligence: 2 of 3 sources complete · 1 partial". */
export function coverageHeadline(summary: Pick<AnalysisSummary, 'overall' | 'sources'>): string {
  const done = summary.sources.filter(sourceComplete).length;
  const tally = new Map<string, number>();
  for (const source of summary.sources) {
    if (sourceComplete(source)) continue;
    const text = sourceStateText(source.state);
    tally.set(text, (tally.get(text) ?? 0) + 1);
  }
  const rest = [...tally].map(([text, n]) => `${n} ${text}`);
  return [`${done} of ${summary.sources.length} sources complete`, ...rest].join(' · ');
}

/**
 * What an empty list of findings for some scope means. Only complete coverage
 * may say "none found"; incomplete or pending coverage never reads as clear.
 */
export function emptyFindingsMessage(summary: Pick<AnalysisSummary, 'overall'>, scope = 'this file'): string {
  switch (coverageTone(summary)) {
    case 'complete':
      return `No findings for ${scope}. Every analysis source completed.`;
    case 'pending':
      return `No findings for ${scope} yet. Analysis is still running.`;
    default:
      return `No findings for ${scope} from the sources that ran, but coverage is incomplete. This is not an all-clear.`;
  }
}

export function verdictMessage(verdict: QueueVerdict): string {
  switch (verdict.kind) {
    case 'remaining':
      return `${verdict.count} ${verdict.count === 1 ? 'file' : 'files'} remaining in the suggested order.`;
    case 'all_clear':
      return 'Nothing remains and every source completed.';
    case 'empty_pending':
      return 'Nothing remains so far, but analysis is still running.';
    case 'empty_incomplete':
      return 'Nothing remains, but some sources are partial, failed or did not run. This is not an all-clear.';
    default:
      return 'Queue state is unknown.';
  }
}

/**
 * Labels for the #30 reason codes. A transliteration of each code, not new
 * prose: the cited finding, thread, rule and provider are shown alongside.
 */
const REASON_TEXT: Record<ReasonCode, string> = {
  unreviewed: 'Not reviewed yet',
  changed_since_reviewed: 'Changed since you reviewed it',
  reviewed_current: 'Reviewed at the current diff',
  excluded_by_rule: 'Excluded by a repository rule',
  unresolved_thread: 'Unresolved GitHub thread',
  outdated_unresolved_thread: 'Unresolved GitHub thread on outdated code',
  confirmed_failure: 'Confirmed test or check failure',
  validated_high_attention: 'Validated high-attention finding',
  deterministic_high_attention: 'High attention from a deterministic rule',
  finding_attention: 'Finding worth investigating',
};

export function reasonText(code: ReasonCode | string): string {
  return REASON_TEXT[code as ReasonCode] ?? `Reason "${code}"`;
}

export type ReasonView = {
  text: string;
  /** Cited source details, each a concrete identifier. */
  cites: string[];
  findingId: string | null;
  threadId: string | null;
};

export function describeReason(reason: Reason, findings: Map<string, ReviewFinding>, threads: ReviewThread[]): ReasonView {
  const cites: string[] = [];
  const finding = reason.findingId ? findings.get(reason.findingId) : undefined;
  if (finding) cites.push(`finding “${finding.title}”`);
  else if (reason.findingId) cites.push(`finding ${reason.findingId}`);
  if (reason.providerId) cites.push(`provider ${reason.providerId}`);
  if (reason.ruleId) cites.push(reason.code === 'excluded_by_rule' ? `exclusion rule ${reason.ruleId}` : `rule ${reason.ruleId}`);
  const thread = reason.threadId ? threads.find((t) => t.githubThreadId === reason.threadId) : undefined;
  if (thread) {
    const author = thread.comments[0]?.author ?? 'unknown author';
    cites.push(`thread by ${author}${thread.line !== null ? ` on line ${thread.line}` : ''}`);
  } else if (reason.threadId) {
    cites.push(`thread ${reason.threadId}`);
  }
  if (reason.evidenceIds.length) cites.push(`${reason.evidenceIds.length} evidence ${reason.evidenceIds.length === 1 ? 'record' : 'records'}`);
  return { text: reasonText(reason.code), cites, findingId: reason.findingId, threadId: thread?.id ?? null };
}

export type Indicator = { key: string; glyph: string; short: string; label: string };

/** Compact file-tree indicators. Each has visible text and a full label. */
export function fileIndicators(item: QueueItem | undefined): Indicator[] {
  if (!item) return [];
  const out: Indicator[] = [];
  if (item.scope.ignored) out.push({ key: 'ignored', glyph: '⊘', short: 'ignored', label: 'Ignored by a repository rule' });
  if (item.attention === 'high') out.push({ key: 'attention', glyph: '▲', short: 'high', label: 'High attention' });
  else if (item.attention === 'investigate') out.push({ key: 'attention', glyph: '△', short: 'look', label: 'Worth investigating' });
  const open = item.reasons.filter((r) => r.code === 'unresolved_thread').length;
  if (open) out.push({ key: 'threads', glyph: '💬', short: String(open), label: `${open} unresolved GitHub ${open === 1 ? 'thread' : 'threads'}` });
  const failures = item.reasons.filter((r) => r.code === 'confirmed_failure').length;
  if (failures) out.push({ key: 'failure', glyph: '✕', short: String(failures), label: `${failures} confirmed ${failures === 1 ? 'failure' : 'failures'}` });
  return out;
}

export type QueueFilter = 'remaining' | 'all' | 'high' | 'feedback' | 'reviewed' | 'ignored';

export const QUEUE_FILTERS: { value: QueueFilter; label: string }[] = [
  { value: 'remaining', label: 'Needs review' },
  { value: 'all', label: 'All files' },
  { value: 'high', label: 'High attention' },
  { value: 'feedback', label: 'Unresolved GitHub feedback' },
  { value: 'reviewed', label: 'Reviewed' },
  { value: 'ignored', label: 'Ignored' },
];

const REMAINING = new Set(['changed_since_reviewed_with_feedback', 'unreviewed_with_feedback', 'high_deterministic_attention', 'needs_review']);

/**
 * Filters the server-ordered queue without reordering it. Ignored files are
 * hidden unless revealed, except under the "Ignored" filter itself.
 */
export function filterQueue(items: QueueItem[], filter: QueueFilter, showIgnored: boolean): QueueItem[] {
  return items.filter((item) => {
    const ignored = item.bucket === 'ignored';
    if (ignored && !showIgnored && filter !== 'ignored') return false;
    switch (filter) {
      case 'remaining': return REMAINING.has(item.bucket) || (ignored && showIgnored);
      case 'all': return true;
      case 'high': return item.attention === 'high';
      case 'feedback': return item.reasons.some((r) => r.code === 'unresolved_thread' || r.code === 'outdated_unresolved_thread');
      case 'reviewed': return item.bucket === 'reviewed';
      case 'ignored': return ignored;
      default: return true;
    }
  });
}

export type FindingFilter = 'all' | InsightClass;

/** Current findings anchored to a file, in server order. */
export function findingsForFile(items: FindingItem[], fileId: string, path: string, filter: FindingFilter = 'all'): FindingItem[] {
  return items.filter((item) => {
    const anchor = item.finding.anchor;
    if (item.label !== 'current') return false;
    if (anchor.fileId !== fileId && anchor.currentPath !== path) return false;
    return filter === 'all' || presentAssertion(item.finding.assertionKind, item.finding.validation.status).cls === filter;
  });
}

/** Findings that project a native GitHub thread already shown as a thread. */
export function isThreadProjection(finding: ReviewFinding, threads: ReviewThread[]): boolean {
  const id = finding.anchor.githubThreadId;
  return id !== null && threads.some((t) => t.githubThreadId === id);
}

export function citedLines(finding: Pick<ReviewFinding, 'anchor'>): string {
  const a = finding.anchor;
  const side = a.side === 'LEFT' ? 'old' : a.side === 'RIGHT' ? 'new' : null;
  if (a.line !== null) {
    const range = a.startLine !== null && a.startLine !== a.line ? `lines ${a.startLine}–${a.line}` : `line ${a.line}`;
    return `${side ? `${side} ` : ''}${range}${a.currentPath ? ` of ${a.currentPath}` : ''}`;
  }
  if (a.kind === 'pr') return 'whole pull request';
  return a.currentPath ? `whole file ${a.currentPath}` : `${a.kind} anchor`;
}

/** Evidence value with unit and denominator, never a derived percentage. */
export function evidenceValue(evidence: Evidence): string {
  const parts: string[] = [];
  if (evidence.value !== null && evidence.value !== undefined) {
    const raw = typeof evidence.value === 'object' ? JSON.stringify(evidence.value) : String(evidence.value);
    parts.push(evidence.unit ? `${raw} ${evidence.unit}` : raw);
  }
  if (evidence.numerator !== null && evidence.denominator !== null) parts.push(`${evidence.numerator} of ${evidence.denominator}`);
  return parts.join(' · ');
}

export function ruleText(finding: ReviewFinding): string | null {
  const s = finding.source;
  if (!s.ruleId) return null;
  return s.ruleVersion ? `${s.ruleId} v${s.ruleVersion}` : s.ruleId;
}

export function providerText(finding: ReviewFinding): string {
  const s = finding.source;
  const parts = [s.providerId];
  if (s.githubActor) parts.push(`by ${s.githubActor}`);
  if (s.modelId) parts.push(`model ${s.modelId}${s.modelRevision ? `@${s.modelRevision}` : ''}`);
  return parts.join(' · ');
}
