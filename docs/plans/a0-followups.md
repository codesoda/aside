# A0 follow-up slice

Issues: #64, #65, #66 — the latent defects and design debt found while reviewing
the A0 slice, filed as it merged (PR #45, now on `main`).

## Model

Same as the A0 slice: integration branch `feat/a0-followups` off `main`, one
issue per branch, PRs stack onto the integration branch, one final PR → `main`
that kivlor merges. Sub-agent PRs never merge or approve; the orchestrator
reviews each.

## Issues

| Issue | Title | Files | Notes |
| --- | --- | --- | --- |
| #66 | Empty source list reports Complete | `src/model/lifecycle.rs`, `src/priority.rs` | Tiny. Do FIRST: settles the lifecycle semantics #65 presents through. |
| #65 | Generated/oversized files skipped without a reason code | `src/diff/facts/`, `src/model/lifecycle.rs`, presentation | Biggest. Touches run-level presentation, so it follows #66. **Changes scan output → `ENGINE_VERSION` must be bumped** (see `config_digest_changes_with_engine_version`); regenerate goldens. |
| #64 | Keyset pagination to replace strict cursor invalidation | `src/server/analysis.rs`, `src/store/analysis.rs`, `app/src/insights/` | Independent of the other two. Acceptance includes simplifying/removing #38's bounded-restart workaround. |

## Sequencing

1. **#66** — alone, first. `lifecycle::overall(&[])` semantics are the
   foundation both #65's presentation and the Review screen build on.
2. **#65** — after #66 (shares the lifecycle/presentation seam).
3. **#64** — independent (server + app only); parallel-safe with #65 but run
   last to keep the seam ownership simple.

## Standing constraints (carried from the A0 slice)

- `git commit -s`; `cargo fmt --all`; `cargo clippy --all-targets -- -D warnings`;
  `cargo test --locked`; `pnpm -C app check`. Never weaken/skip/delete/`#[ignore]`
  tests; test count never goes down. Baseline on `main`: 401 unit + 2 integration.
- No credentials, session descriptors, or private PR content in commits.
- `ENGINE_VERSION` must be bumped on any scan-output change (#65 will).
- AGENTS.md issue workflow: plan comment before implementation, progress
  comments, `Fixes #ISSUE` in PR descriptions, stacked PRs must name their base.

## Status

- [ ] #66 — assigned, not started
- [ ] #65 — assigned, not started
- [ ] #64 — assigned, not started
