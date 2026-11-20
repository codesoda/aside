# A0 review-intelligence integration branch

This branch (`feat/a0-review-intelligence`) is the shared base for the A0 slice of
the review-intelligence roadmap. Sub-agent branches stack on top of it and open
PRs **against this branch**, not `main`. `main` receives one final merge once the
slice is complete.

## Scope

| Issue | Title | Depends on | Branch | Status |
| --- | --- | --- | --- | --- |
| [#15](https://github.com/codesoda/aside/issues/15) | Define Rust analysis, finding, evidence and imported-source contracts | — | `feat/15-analysis-contracts` | merged (PR #46) |
| [#17](https://github.com/codesoda/aside/issues/17) | Build rich offline intelligence fixtures and contract tests | #15 | `feat/17-offline-fixtures` | merged (PR #49) |
| [#18](https://github.com/codesoda/aside/issues/18) | Persist analysis runs, evidence, source records and user triage | #15 | `feat/18-analysis-persistence` | merged (PR #48) |
| [#22](https://github.com/codesoda/aside/issues/22) | Schedule parallel bounded providers without blocking Review | #15, #18 | `feat/22-provider-scheduler` | merged (PR #52) |
| [#19](https://github.com/codesoda/aside/issues/19) | Generate versioned deterministic diff facts with evidence | #15, [#14](https://github.com/codesoda/aside/issues/14) | `feat/19-diff-facts` | merged (PR #51) |

#14 is a P0 safety ticket outside this slice. It landed on `main` in PR #47 and
was merged down into this branch, so #19 is unblocked.

## Order of work

1. **Wave 1** — #15 alone. Everything else consumes its types.
2. **Wave 2** — #17 and #18 in parallel, once #15 is merged into this branch.
3. **Wave 3** — #22 (after #18) and #19 (after #14 merged down), in parallel. Done.

## Open gap: the diff engine is not registered

#19 and #22 were built in parallel with deliberately disjoint scope: #19 was told
not to implement scheduling, #22 was told not to implement engines. Both met their
own acceptance criteria, but nothing joins them. `analysis::default_providers()`
returns an empty `Vec`, and `diff::facts::scan` is called only from its own tests,
so a running Aside schedules no providers and produces no findings.

Closing this needs an `AnalysisProvider` implementation that wraps
`diff::facts::scan` and is registered in `default_providers()`. It is small, but
it is not inside any of the five issues in this slice, so it is left as an
explicit decision rather than silently absorbed: either a wiring commit on this
branch or a new issue before the slice merges to `main`.

## Carried-forward notes

- The #15 contracts use `#[serde(other)]` on seven enums, so an unrecognised
  value decodes to `unknown` and loses the original string. #18 contains the
  damage by persisting the raw source JSON alongside decoded columns; anything
  that round-trips these records must do the same.
- #17's offline test proves the demo makes no *proxied* network calls plus a
  static source guard. It is not a sandbox-level guarantee.
- `RevisionConflict` from #18 needs to surface as HTTP 409 in #33.
- Retention policy lives in the store (5 snapshots); #34 may want it configurable.

## Rules for branches on this slice

- Branch from the current tip of `feat/a0-review-intelligence`; never from `main`.
- One issue per branch, one PR per branch, targeting `feat/a0-review-intelligence`.
- Sign off every commit (`git commit -s`), per `CONTRIBUTING.md`.
- `cargo fmt`, `cargo clippy --all-targets`, and `cargo test --locked` must pass.
- No credentials, session descriptors, or private PR content in commits.
- The demo path stays offline: no network or `gh` required for `aside demo`.
