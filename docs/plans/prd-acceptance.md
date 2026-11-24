# PRD §9 backend acceptance

Issue: [#21](https://github.com/codesoda/aside/issues/21). This maps the original product-behaviour cases to the test that covers them. It does not add a new product surface.

| §9.1 case | Where it is enforced |
| --- | --- |
| 1. CLI launch of a live PR | Not automated. Requires `gh` auth and a repository the reviewer is allowed to read. |
| 2. Offline demo, no browser | `aside demo --no-open` in `tests/embedded_asset_smoke.sh`. `src/demo.rs` checks the bundled capture. |
| 3. Recursive directory exclusion | `exclusions_and_digests_survive_restart_and_stay_scoped` and `rules::directory_rules_match_descendants_only`. |
| 4. File exception inside an exclusion | Same acceptance test; `rules::prd_precedence_example`. |
| 5. Pattern exclusion | `rules::glob_semantics`. |
| 6. Disabling or deleting a rule restores scope | `ignoring_and_reincluding_keeps_acknowledgements`; `rules::disabled_rules_are_skipped`. |
| 7. Repository rules persist across restart and the next PR | `exclusions_and_digests_survive_restart_and_stay_scoped`; `store::rules::review_rules_survive_restart_and_are_isolated_and_last`. |
| 8. Exclusions do not change GitHub, `.gitignore`, or a checkout | Exclusion routes only write SQLite (`src/server/exclusions.rs`). Publication tests use a fake `gh` runner and never touch a working tree. |
| 9. Review acknowledgement persists | `ignoring_and_reincluding_keeps_acknowledgements`. |
| 10. Unchanged file stays reviewed after a new head | `exclusions_and_digests_survive_restart_and_stay_scoped` using `demo::snapshot(1)`. |
| 11. Changed patch is changed-since-reviewed | Same test; `session::refresh_keeps_unchanged_reviews_and_flags_changed_diffs`. |
| 12. Ignore and re-include keep acknowledgement history | `ignoring_and_reincluding_keeps_acknowledgements`. |
| 13. Progress buckets do not overlap | `assert_partition` in the restart acceptance test. |
| 14. Diff fidelity | `diff::parser` tests for renames, deletions, binaries, modes, CRLF, and line lookup. |
| 15. Stale anchors | `session::refresh_revalidates_local_anchors_without_moving_them` and `session::absent_file_orphans_anchor`. |
| 16. Snapshot plus events, reconnect | `server::reconnect_behind_the_current_sequence_starts_with_resync` and `server::live_events_carry_their_sequence_as_the_sse_id`. |
| 17. Local comments are not GitHub writes | `server::threads` private-conversation coverage and `publish` tests that fail before a remote write. |
| 18. Safe publication | `publish::base_only_change_fails_before_claim_or_write`, deleted-anchor, incomplete-snapshot, and draft-selection tests. |
| 19. One binary serves the UI without Node | `server::assets::embedded_index_is_served_and_dot_paths_are_rejected` plus `tests/embedded_asset_smoke.sh` on the release binary. |
| 20. Agent skill | `skill::installs_and_replaces_only_its_own_skill` and the embedded skill text test. |

## Release smoke

`tests/embedded_asset_smoke.sh` starts `aside demo --no-open` with isolated `ASIDE_DATA_DIR` and `ASIDE_CONFIG_DIR`, removes Node entries from `PATH`, and requires the served HTML to contain `id="app"` and not the build-script placeholder. A dot-path request must be 404. Pull requests run this on the Linux release binary. The tag workflow runs it on each release target before packaging.

The script does not print the private login URL. On failure it redacts `token=` values from stderr.

## Limitations

- `aside demo` keeps using an in-memory database, so a demo process does not persist exclusions or acknowledgements. Persistence applies to live sessions, which this matrix opens through `Store::open`.
- Live `aside <pr-url>` and a real GitHub publication are not part of CI. Do not point publication tests at someone else's pull request.
- There is no browser automation run. Frontend component tests cover the review UI separately.
- The pull-request smoke is Linux x86-64 only. macOS ARM, Windows x86-64, and Windows ARM run the same script in `.github/workflows/release.yml` when a `v*` tag is pushed.
- The Windows installer (`install.ps1`) is still outside the Unix installer suite.
