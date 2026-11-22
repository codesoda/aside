# Pinned Git history measurements

## Temporal co-change inspection hints

The same pinned history read also measures exact-path co-change transactions.
This is independent of Rust parsing and percentile availability: small and
non-Rust repositories can produce useful associations without a churn rank.

A transaction is one non-merge commit inside the inclusive baseline window.
Distinct commit identities remain distinct even when timestamps match. Empty
commits are eligible transactions but add no path support. Commits touching
**more than 50 distinct paths** are excluded from co-change, **before** filtering
against the baseline tree: deleted and non-baseline paths count toward breadth.
These bulk commits still count for churn. No history sampling is performed.

For a changed baseline path A and a baseline partner B absent from the PR:

- `supportAB`: eligible transactions containing both paths;
- `supportA` / `supportB`: eligible transactions containing each path;
- A→B confidence: `supportAB / supportA`; reverse: `supportAB / supportB`.

A hint requires `supportAB >= 5` and A→B confidence **at least 0.6** (inclusive,
compared with integer arithmetic). Reverse confidence is reported separately;
A→B does not imply B→A. These engineering thresholds are not calibrated defect
probabilities. Hints say to inspect a related file, never that code is missing.
They are informational-severity attention hints with at most Investigate priority.

Both current and previous PR paths are excluded as partners. A renamed source
uses its previous baseline path but anchors to its current PR file. A partner is
related baseline context, not an invented PR file or line. Historically ambiguous
identities are excluded; renames are not followed or reconstructed.

Corpus evidence includes the comparison base, inclusive window, walked and
counted commits, merge/bulk exclusions, eligible-transaction denominator,
thresholds, method version and unsampled status. Association evidence adds both
directional ratios and supports, up to five lexically first `(commit SHA,
committer timestamp)` examples, and qualifying/emitted output counts. Examples
are illustrations, not the population used for measurement. Merge-exclusion
counts cover the whole bounded traversal; counted/eligible transactions cover
only the declared window.

Only pairs involving changed baseline paths are tracked, with a hard cap of
50,000 candidate pairs. Hitting that cap produces unknown co-change coverage and
**no** association hints rather than biased partial support. The traversal's
existing commit/object/deadline bounds still fail closed. Incomplete context,
missing baseline paths and ambiguous identities remain unknown, not verified
zero findings. No eligible history likewise has unknown association coverage.

Hints sort by support descending, then A→B confidence descending, then exact
source/partner paths. At most 100 are emitted, further restricted by the
provider's remaining finding budget. Truncation is explicitly partial coverage
with `cochange_output_truncated`; it is output truncation, not history sampling.
Co-change cancellation is checked during counting and emission; interrupted
measurement emits no co-change hints. The engine/configuration version changes
with this rule, preventing reuse of old provider results.

## Churn and complexity

The `aside.git-history` provider uses an immutable copy of validated Git objects. It reads the captured comparison-base commit, not PR commits or the current checkout. When Git history is unavailable, the provider reports partial coverage rather than a clean result. Incomplete reads keep a structured run warning with a reason, pinned base, available tree-path and walked/counted/merge counts, any known anchor/window, and the configured resource limits. These counts describe only work completed before failure; no rank or hotspot score is emitted. History failures do not stop diff facts.

The baseline commit's **committer timestamp** anchors an inclusive 180-day window. Commits after that timestamp do not count. Git traversal inspects at most 1,001 commits, independent of timestamp ordering. A history cap, shallow boundary, missing object, malformed Git response, output limit or deadline makes the population incomplete. Merge commits contribute no changed paths; the evidence reports how many were skipped.

A successful read always persists a PR-level informational corpus finding with linked evidence for its base, anchor/window, walked/counted/merge-skipped totals, baseline size, and parser population. This remains available for addition-only changes and empty baseline trees; it does not imply a rank or a defect. A touching commit contributes one count per path, regardless of line count. Days since change uses the newest eligible timestamp. The population consists of regular files in the baseline tree, including those with zero touches. A percentile is `100 × (population files with a strictly smaller count) / population size`; the numerator and denominator accompany measurements. Fewer than 20 population members cannot establish an attention rank. Path identity is exact: historical renames are **not** followed. A changed PR file with a previous path uses that previous path. A path deleted and reintroduced in the historical window makes ranking ambiguous; the provider suppresses ranks instead of guessing lineage.

Rust complexity is measured only on baseline `.rs` files touched in the window. The parser is tree-sitter 0.27.2 with tree-sitter-rust 0.24.2 (MIT licensed). It reports the largest function's cyclomatic measure, starting at one and counting `if`, `while`, `for`, `?`, non-first match arms and `&&`/`||`; macros are excluded. Files that fail parsing remain in the Rust population denominator, but no complexity rank is published from an incomplete parser population. Unsupported source languages have unknown complexity; non-source files have no applicable complexity. Parser failure does not discard valid churn counts.

A churn attention hint requires a churn percentile of at least 90 and at least five touching commits. A hotspot hint requires both churn and complexity percentiles of at least 75 and at least five commits. The hotspot index is `churn percentile × complexity percentile / 100`. These are review-priority hints, never defect assertions, and the maximum attention level is Investigate.

Bounds: Git log/tree/diff output is limited to 16 MiB per read; tree and diff walks to 200,000 paths; blobs to 512 KiB; historical traversal to 1,000 commits; Rust parser population to 500 files; and shared history work to 75 seconds. Git subprocesses are killed and reaped when their output or deadline limit trips. Tree-sitter's parse budget is **cooperative** (250 ms), not a hard real-time limit; cancelled or late parser results are not published as successful measurements. Evidence and presentation findings are scoped to the producing run, so a new head cannot reuse a stale finding anchor or evidence link. Pruning an old snapshot retains only current-run evidence. Cancellation is cooperative between Git commands, parser measurements, rank entries, and emitted files; a Git command already in progress may continue until its bounded deadline. A separate reaper joins a cancelled worker without blocking the Tokio executor.
