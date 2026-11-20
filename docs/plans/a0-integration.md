# A0 review-intelligence integration branch

This branch (`feat/a0-review-intelligence`) is the shared base for the A0 slice of
the review-intelligence roadmap. Sub-agent branches stack on top of it and open
PRs **against this branch**, not `main`. `main` receives one final merge once the
slice is complete.

## Scope

| Issue | Title | Depends on | Branch | Status |
| --- | --- | --- | --- | --- |
| [#15](https://github.com/codesoda/aside/issues/15) | Define Rust analysis, finding, evidence and imported-source contracts | — | `feat/15-analysis-contracts` | not started |
| [#17](https://github.com/codesoda/aside/issues/17) | Build rich offline intelligence fixtures and contract tests | #15 | `feat/17-offline-fixtures` | blocked |
| [#18](https://github.com/codesoda/aside/issues/18) | Persist analysis runs, evidence, source records and user triage | #15 | `feat/18-analysis-persistence` | blocked |
| [#22](https://github.com/codesoda/aside/issues/22) | Schedule parallel bounded providers without blocking Review | #15, #18 | `feat/22-provider-scheduler` | blocked |
| [#19](https://github.com/codesoda/aside/issues/19) | Generate versioned deterministic diff facts with evidence | #15, [#14](https://github.com/codesoda/aside/issues/14) | `feat/19-diff-facts` | blocked (external: #14) |

#14 is a P0 safety ticket outside this slice, so #19 cannot complete here until it
lands.

## Order of work

1. **Wave 1** — #15 alone. Everything else consumes its types.
2. **Wave 2** — #17 and #18 in parallel, once #15 is merged into this branch.
3. **Wave 3** — #22, once #18 is merged into this branch.
4. **Deferred** — #19, once #14 lands on `main` and is merged down.

## Rules for branches on this slice

- Branch from the current tip of `feat/a0-review-intelligence`; never from `main`.
- One issue per branch, one PR per branch, targeting `feat/a0-review-intelligence`.
- Sign off every commit (`git commit -s`), per `CONTRIBUTING.md`.
- `cargo fmt`, `cargo clippy --all-targets`, and `cargo test --locked` must pass.
- No credentials, session descriptors, or private PR content in commits.
- The demo path stays offline: no network or `gh` required for `aside demo`.
