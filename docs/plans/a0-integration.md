# A0 review-intelligence integration branch

This branch (`feat/a0-review-intelligence`) is the shared base for the A0 slice of
the review-intelligence roadmap. Sub-agent branches stack on top of it and open
PRs **against this branch**, not `main`. `main` receives one final merge once the
slice is complete.

## Scope

All A0 work is complete and merged into this branch.

| Issue | Title | PR |
| --- | --- | --- |
| [#15](https://github.com/codesoda/aside/issues/15) | Define Rust analysis, finding, evidence and imported-source contracts | #46 |
| [#18](https://github.com/codesoda/aside/issues/18) | Persist analysis runs, evidence, source records and user triage | #48 |
| [#17](https://github.com/codesoda/aside/issues/17) | Build rich offline intelligence fixtures and contract tests | #49 |
| [#19](https://github.com/codesoda/aside/issues/19) | Generate versioned deterministic diff facts with evidence | #51 |
| [#22](https://github.com/codesoda/aside/issues/22) | Schedule parallel bounded providers without blocking Review | #52 |
| [#23](https://github.com/codesoda/aside/issues/23) | Paginate GitHub threads and every nested reply connection | #55 |
| [#27](https://github.com/codesoda/aside/issues/27) | Implement invalidation, coverage and SSE run lifecycle | #56 |
| [#33](https://github.com/codesoda/aside/issues/33) | Expose paginated authenticated analysis and feedback APIs | #58 |
| [#30](https://github.com/codesoda/aside/issues/30) | Define explainable priority buckets and scope-aware counts | #59 |
| [#36](https://github.com/codesoda/aside/issues/36) | Test reproducibility, partial coverage and false-positive boundaries | #62 |
| [#38](https://github.com/codesoda/aside/issues/38) | Render progressive insights on existing Review screen | #63 |

Plus two integration fixes that belonged to no issue:

| Change | PR |
| --- | --- |
| Register the diff-facts engine as an analysis provider (seam between #19 and #22) | #54 |
| Replace prose-matched import problems with a typed enum (seam between #23 and #27) | #60 |

[#14](https://github.com/codesoda/aside/issues/14) is a P0 safety ticket outside
this slice. It landed on `main` in PR #47 and was merged down here, unblocking #19.
[#23](https://github.com/codesoda/aside/issues/23) is A1, pulled in because it
gates #30.

## Order of work

1. **Wave 1** — #15 alone; everything consumes its types.
2. **Wave 2** — #17, #18 in parallel.
3. **Wave 3** — #19, #22 in parallel.
4. **Wave 4** — diff-provider wiring, #23 in parallel.
5. **Wave 5** — #27 alone; everything remaining depended on it.
6. **Wave 6** — #30, #33 in parallel.
7. **Wave 7** — #36, #38, typed-problem fix in parallel.

## Recurring lesson: disjointly scoped parallel issues leave the join unowned

Three times in this slice, two issues were implemented in parallel with correctly
disjoint scope, each met its acceptance criteria, and the thing connecting them
belonged to neither: #19/#22 (engine never registered), #23/#27 (typed problems
flattened to prose and re-parsed), #30/#33 (ranking had no wire format, so the UI
could not read it; closed inside #38). Scope boundaries that prevent conflicts
also create seams. Future parallel waves should name an owner for the join.

## Carried-forward notes

- The #15 contracts use `#[serde(other)]` on seven enums, so an unrecognised value
  decodes to `unknown` and the original string is lost. Three containment patterns
  are now in use, each deliberately chosen: `Raw<T>` (#18) for data GitHub owns,
  plain strings at the import boundary (#23), and a closed typed enum (#60) for
  values Aside produces itself.
- #17's offline test proves the demo makes no *proxied* network calls plus a static
  source guard. It is not a sandbox-level guarantee.
- #33 uses strict cursor invalidation: any change to a result set invalidates
  outstanding cursors. Safe, but during a live run the scheduler streams findings
  in, so clients paging mid-run will hit repeated recoverable 409s. #38 handles it
  by restarting from page one. The durable fix is keyset pagination on a stable
  sort key, leaving 409 for genuine snapshot changes.
- Retention (5 snapshots) and the #19 thresholds (400 changed lines, 10,000 diff
  lines, 20 evidence records) are uncalibrated defaults — #34 for configurability,
  #44 for calibration.
- `ENGINE_VERSION` is hashed into the config digest the scheduler uses to skip
  already-run providers. Any change to scan output must bump it, or existing
  databases keep serving stale results. `config_digest_changes_with_engine_version`
  guards this.

## Known latent issues, not yet filed

- `lifecycle::overall(&[])` returns `Complete`, so a session with no sources would
  read as all-clear. Unreachable today because sessions always carry import
  sources, but the invariant rests on callers rather than on the type.
- Generated and oversized files are recorded `not_run` but contribute no reason
  code, so a run containing them can still present as complete/succeeded — the
  same false-clean class #36 fixed for mode-only files, one layer up.
- Calibration questions reported by #36 for #44: test moves across files are not
  recognised, an in-place assertion edit counts as both added and removed, a
  docs-only PR is marked partial because Markdown is unsupported, and a pure
  rename of a supported-language file still reports `analyzed{0}`.
- #27 hardcodes reviews and PR comments as incomplete because `gh pr view` cannot
  report page completeness, so the overall verdict never reads complete on a real
  session until #24 lands.
