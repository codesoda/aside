# Boundary fixtures (#36)

Synthetic PRs that pin how deterministic diff facts (#19) behave at their
edges. Nothing here came from a real PR. Tests:
`src/diff/facts/boundary_tests.rs`.

Each `boundary-*` / `test-gap-*` directory contains:

| File | Contents |
|---|---|
| `manifest.json` | Snapshot identity, `snapshotComplete`, `unsupportedPaths` and `expectedCompleteness` |
| `pr.diff` | Unified diff, parsed with the production parser |
| `expected-facts.json` | Golden, byte-exact `DiffFacts`. Regenerate with `ASIDE_UPDATE_GOLDEN=1 cargo test` and review the diff |
| `expected-labels.json` | Hand-written evaluation of every emitted label: `appropriate` or `questionable`, with a note |

`demo-cli-14921/diff-facts-labels.json` evaluates the same way over the
bundled public capture.

| Fixture | Boundary |
|---|---|
| `boundary-renames` | Pure rename, rename with a signature edit, renamed test with an edited assertion |
| `boundary-file-kinds` | Deleted source, deleted/added/modified binaries, mode-only (Rust and shell) |
| `boundary-truncated` | Unverified snapshot plus a patch truncated mid-hunk |
| `boundary-mixed` | Docs, binary, mode-only, Go code and test, deleted Ruby, rename with edit, truncated patch |
| `test-gap-tests-elsewhere` | Code change, tests live in an unchanged file |
| `test-gap-moved-tests` | Inline tests moved to `tests/` |
| `test-gap-docs-only` | Markdown only |
| `test-gap-comment-only` | Rust comments only |

## What the tests guarantee

- **Drift:** goldens, manifests and labels round-trip exactly through typed
  Rust structs. Typed decoding is deliberate: `#[serde(other)]` makes an
  unknown enum value decode as `unknown`, so it re-serialises differently and
  fails as drift.
- **Stability:** byte-identical facts across repeated scans, fresh parses,
  reversed, rotated and shuffled file orders.
- **Unknown, not clean:** files in `unsupportedPaths` never get
  `analyzed { findings: 0 }`, line anchors or zero line counts. Partial scans
  are never `clean`, `lifecycle::overall` is `incomplete`, `scope_counts` is
  not authoritative, and an empty queue is `EmptyIncomplete`, not `AllClear`.
- **No false test-gap claims:** every finding is a measured fact or attention
  hint, never `high`, `test_coverage` or `correctness`, and never uses
  defect wording. Test-related hints carry reasons, evidence, an action and
  the "does not mean coverage was lost" caveat.

## Questionable labels (for calibration in #44)

The labels are safe, but these are noisy. Nothing is tuned here.

1. `aside.diff.tests_removed` raises an `investigate` hint when tests move to
   another file in the same PR (`test-gap-moved-tests`). The added copy is
   measured but the move is not correlated.
2. `aside.diff.tests_removed` and `aside.diff.test_assertions_added` both
   fire when one assertion is edited in place (`boundary-renames`).
3. Coverage, not a label: a docs-only PR is `partial` because Markdown is an
   "unsupported language" for line rules. This is safe but noisy.
4. Coverage, not a label: a pure 100% rename of a supported-language file
   reports `analyzed { findings: 0 }` for content rules, although no content
   was read. An existing #19 test pins this.
5. Coverage, not a label: `generated_file` and `file_too_large` record
   `not_run` per file but do not add a reason code, so the run can still be
   `complete`/`succeeded`.
