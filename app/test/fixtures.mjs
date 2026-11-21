// Small, hand-built API payloads for component tests. Shapes follow the
// ts-rs generated types; values are synthetic.

const file = (id, path, lines) => ({
  id, path, previousPath: null, changeKind: 'modified', oldMode: null, newMode: null, isBinary: false,
  additions: 1, deletions: 0, contentDigest: null, diffDigest: `digest-${id}`, fingerprintVersion: 1, patchComplete: true,
  hunks: [{ id: `${id}-h1`, oldStart: 1, oldCount: 1, newStart: 1, newCount: 2, header: '', lines }],
});

const state = (fileId, path, ignored = false) => ({
  fileId, path, status: ignored ? 'ignored' : 'unreviewed', reviewStatus: 'unreviewed',
  scope: { ignored, effectiveRuleId: ignored ? 'rule-1' : null, matchingRuleIds: ignored ? ['rule-1'] : [] },
  lastReviewedAt: null,
});

export const source = (sourceId, state, extra = {}) => ({
  sourceId, mode: 'static', snapshotId: 'snap-1', runId: null, status: null, completeness: 'not_run',
  items: { state: 'unknown' }, itemKind: 'findings', state, reasonCodes: [], limitations: [], ...extra,
});

export function sessionView({ overall = 'pending', sources } = {}) {
  return {
    schemaVersion: 1, sessionId: 's1', demo: true, sequence: 1, actor: 'human',
    repository: { id: 'r', host: 'github.com', owner: 'o', name: 'n', providerId: null },
    pr: { owner: 'o', repo: 'n', number: 7, title: 'Tidy auth', author: 'alice', baseRef: 'main', headRef: 'feat', headSha: 'abc', baseSha: 'def' },
    files: [
      file('f-auth', 'src/auth.rs', [
        { id: 'l1', kind: 'context', oldLine: 1, newLine: 1, text: 'fn check() {' },
        { id: 'l2', kind: 'addition', oldLine: null, newLine: 2, text: '    verify_token();' },
      ]),
      file('f-gen', 'gen/out.rs', [{ id: 'g1', kind: 'addition', oldLine: null, newLine: 1, text: 'generated();' }]),
    ],
    fileStates: [state('f-auth', 'src/auth.rs'), state('f-gen', 'gen/out.rs', true)],
    progress: { total: 2, inScope: 1, reviewed: 0, changedSinceReviewed: 0, unreviewed: 1, ignored: 1 },
    rules: [],
    threads: [{
      id: 't1', source: 'github', githubThreadId: 'GH_T1', path: 'src/auth.rs', fileId: 'f-auth', side: 'RIGHT', line: 2,
      startLine: null, lineText: null, anchorState: 'current', anchorReason: null, resolved: false, headSha: 'abc', createdAt: '',
      comments: [{ id: 'c1', threadId: 't1', authorKind: 'human', author: 'bob', body: 'Is the token checked twice?', createdAt: '', updatedAt: '', publishState: 'published', githubCommentId: null }],
    }],
    reviews: [{ author: 'carol', state: 'COMMENTED', body: 'Looks close.', submittedAt: null }],
    issueComments: [],
    review: {},
    warnings: [],
    complete: true,
    fetchedAt: '',
    analysis: {
      eventVersion: 1, snapshotId: 'snap-1', overall,
      sources: sources ?? [
        source('github.review_threads', 'has_results', { mode: 'github_import', completeness: 'complete', items: { state: 'known', value: 1 }, itemKind: 'threads' }),
        source('aside.diff-facts', 'pending', { status: 'running', completeness: 'unknown' }),
      ],
      currentFindings: 0, historicalFindings: 0, currentFindingsExcluded: 0,
    },
  };
}

const anchor = (extra = {}) => ({
  kind: 'diff_line', fileId: 'f-auth', currentPath: 'src/auth.rs', previousPath: null, entityId: null, side: 'RIGHT',
  line: 2, startLine: null, originalLine: null, originalStartLine: null, commitSha: null, originalCommitSha: null,
  diffDigest: null, githubThreadId: null, externalUrl: null, ...extra,
});

function finding(id, assertionKind, title, extra = {}) {
  return {
    finding: {
      schemaVersion: 1, id, repositoryId: 'r', prNumber: 7, observedSnapshotId: 'snap-1',
      source: { kind: 'aside_static', providerId: 'aside.diff-facts', runId: 'run-1', ruleId: 'auth_path', ruleVersion: '2', modelId: null, modelRevision: null, promptVersion: null, native: null, githubActor: null },
      assertionKind, kinds: [], title, body: '', rationale: 'Path matches the auth rule.', suggestedAction: null,
      anchor: anchor(), relatedAnchors: [], evidenceIds: [`${id}-ev`],
      attention: { level: 'high', score: 0.97, scoreVersion: 'v', reasons: ['auth path touched'] },
      severity: { normalized: 'unknown', original: null }, confidence: { kind: 'model_self_reported', value: 0.93, calibrationVersion: null },
      validation: { status: 'not_required', method: null, validatedAt: null, note: null },
      applicability: 'current_exact', sourceStatus: 'open', firstSeenAt: '', lastSeenAt: '', externalUrl: null,
      parentFindingId: null, correlationGroupId: null, extensions: {}, ...extra,
    },
    label: 'current', latestSnapshotId: 'snap-1', latestApplicability: 'current_exact', firstSeenAt: '', lastSeenAt: '',
  };
}

export const findings = [
  finding('fnd-measured', 'measured_fact', 'Changes 2 lines in an auth path'),
  finding('fnd-hint', 'attention_hint', 'Consider checking token reuse'),
  finding('fnd-external', 'external_feedback', 'CodeRabbit said tokens may leak', {
    source: { kind: 'github_review_summary', providerId: 'github.reviews', runId: null, ruleId: null, ruleVersion: null, modelId: null, modelRevision: null, promptVersion: null, native: null, githubActor: 'coderabbitai' },
  }),
];

export const evidence = [{
  schemaVersion: 1, id: 'fnd-measured-ev', kind: 'measurement', sourceRunId: 'run-1', sourceEntityId: null, anchor: anchor({ startLine: 1 }),
  label: 'Changed lines', value: 2, unit: 'lines', numerator: 2, denominator: 40, method: 'diff count', inputDigest: null,
  observedAt: '', limitations: ['Whitespace-only changes are counted'],
}];

export function queueView({ coverage = 'complete', verdict = { kind: 'remaining', count: 1 }, genIgnored = true } = {}) {
  return {
    sessionId: 's1', sequence: 2, eventVersion: 1, snapshotId: 'snap-1',
    queue: {
      items: [
        {
          fileId: 'f-auth', path: 'src/auth.rs', bucket: 'unreviewed_with_feedback', acknowledgement: 'unreviewed', attention: 'high',
          scope: { ignored: false, effectiveRuleId: null, matchingRuleIds: [] },
          reasons: [
            { code: 'unresolved_thread', findingId: null, providerId: null, ruleId: null, threadId: 'GH_T1', evidenceIds: [] },
            { code: 'deterministic_high_attention', findingId: 'fnd-measured', providerId: 'aside.diff-facts', ruleId: 'auth_path', threadId: null, evidenceIds: ['fnd-measured-ev'] },
          ],
          findings: [], threads: ['GH_T1'], topics: [],
        },
        {
          fileId: 'f-gen', path: 'gen/out.rs', bucket: genIgnored ? 'ignored' : 'needs_review', acknowledgement: 'unreviewed', attention: 'none',
          scope: { ignored: genIgnored, effectiveRuleId: genIgnored ? 'rule-1' : null, matchingRuleIds: [] },
          reasons: [genIgnored
            ? { code: 'excluded_by_rule', findingId: null, providerId: null, ruleId: 'rule-1', threadId: null, evidenceIds: [] }
            : { code: 'unreviewed', findingId: null, providerId: null, ruleId: null, threadId: null, evidenceIds: [] }],
          findings: [], threads: [], topics: [],
        },
      ],
      prLevelFindings: [], coverage, verdict,
    },
    counts: {
      files: { total: 2, inScope: genIgnored ? 1 : 2, ignored: genIgnored ? 1 : 0 }, reviewedCurrent: 0, changedSinceReviewed: 0, unreviewed: 1,
      threads: { total: 1, inScope: 1, ignored: 0 }, threadsUnresolvedCurrent: 1, threadsUnresolvedOutdated: 0, threadsResolved: 0,
      currentFindings: { total: 3, inScope: 3, ignored: 0 }, historicalFindings: { total: 0, inScope: 0, ignored: 0 }, coverage,
    },
  };
}

export function page(items, coverage = { complete: true, sources: [] }) {
  return { eventVersion: 1, snapshotId: 'snap-1', sequence: 2, items, nextCursor: null, hasMore: false, total: items.length, limit: 200, includeIgnored: true, excludedByScope: 0, coverage };
}
