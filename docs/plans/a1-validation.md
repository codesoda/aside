# A1 security, reliability and release validation

Issue: [#43](https://github.com/codesoda/aside/issues/43). Counts below are assertions in the named tests, not estimates.

## Measured in this change

| Check | Count | Test |
| --- | --- | --- |
| Cross-origin session read has no CORS allow header | 1 response | `a1_validation::cross_origin_writes_fail_and_responses_are_not_shared` |
| Cross-origin exclusion write rejected | 1 forbidden response, error `invalid_origin` | same |
| Rules left after that rejected write | 0 | same |
| Untrusted comment body stored unchanged | 1 comment, exact payload | `a1_validation::untrusted_comment_and_path_stay_data` |
| Response bytes containing a local password-file marker | 0 | same |
| Traversal exclusion `../etc/passwd` | 1 bad request | same |
| Hostile diff path kept as data | 1 file, path `../etc/passwd`, 0 warnings | same |
| Default diff file cap | 3000 | `a1_validation::oversized_diff_keeps_a_counted_prefix` |
| Diff over the cap | 2 files kept, 1 warning, omitted file absent | same |
| Demo session GitHub runner | none | `a1_validation::demo_session_has_no_github_runner` |
| Rendered `<script>` and `<img>` elements from a comment | 0; text and `&lt;script&gt;` remain | `conversationPanel.test.mjs` |

The release binary smoke from #21 remains the bundled-CLI check: `tests/embedded_asset_smoke.sh`.

## Already covered elsewhere

- Loopback `Host` rejection, missing `Origin` on writes, and agent publication denial: `server::auth_and_permissions`, `server::loopback_hosts_only`.
- Stale fingerprint and anchor revalidation: `server::stale_digest_and_host_rejection`, `publish` base/anchor tests, `session::refresh_revalidates_local_anchors_without_moving_them`.
- Pagination over 100 reviews and partial pages: `github::feedback_tests`.
- Rate-limit and forbidden GitHub errors stay partial rather than zero: `github::classify_errors_without_echoing_secrets`, `github::checks_tests`, demo scenario `s17`.
- Schema migration and reopen: `store::migrations_apply_and_reopen`.
- SSE resume and live sequence ids: `server::reconnect_behind_the_current_sequence_starts_with_resync`, `server::live_events_carry_their_sequence_as_the_sse_id`.
- analyze.md §19.5 scenarios s01, s03–s08, s10–s13, s16–s18: `demo::intelligence_tests`.

## Not run here

- A live pull request was not fetched or evaluated. That remains opt-in and must not run against someone else's repository.
- Optional LLM scenarios in §19.4, other than "off by default", stay deferred by #39 and #42.
- Ranking calibration in §19.6 is #44.
- Browser scroll, focus, and virtualisation are not automated. Comment text is covered by the React test above; there is no Markdown or raw-HTML renderer in the review UI.
