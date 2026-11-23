# Repository analysis controls

For the deferred, default-off optional review-analysis LLM decision and future
consent/budget requirements, see [LLM provider decision](llm-provider-decision.md).
This does not change the existing on-demand Explain feature.

Analysis preferences live in Aside's OS-local `config.toml` (or the directory
selected by `ASIDE_CONFIG_DIR`). They are not read from an untrusted checkout,
posted to GitHub, or copied into PR snapshots/SQLite. Run identities store only a
hash of the resolved policy, engine configuration and limits.

Use an explicit host-qualified repository; the working directory is never used:

```sh
aside get --repo github.com/owner/repo
aside set analysis.history.enabled false --repo github.com/owner/repo
aside set analysis.diff.max_input_bytes 1048576 --repo github.com/owner/repo
aside set analysis.history.timeout_ms 30000 --repo github.com/owner/repo
aside set analysis.history.timeout_ms default --repo github.com/owner/repo
```

Repository names are normalized to lowercase. A different host or repository
gets independent settings. A renamed repository needs its preference entry moved
explicitly; settings do not follow GitHub numeric IDs or transfer to another name.

Existing sessions apply changes on **Refresh**. Invalid configuration aborts the
refresh before network calls or state changes; fix it and refresh again. No new
settings HTTP API is exposed, and agents do not gain analysis-control rights.

## Settings and limits

Both `analysis.diff` and `analysis.history` accept:

| Setting | Default | Allowed override |
| --- | --- | --- |
| `enabled` | `true` | `true` / `false` |
| `timeout_ms` | engine/scheduler default | 1–600,000 ms |
| `max_input_bytes` | engine/scheduler default | 1–268,435,456 diff bytes |
| `max_input_files` | engine/scheduler default | 1–100,000 changed files |
| `max_findings` | engine/scheduler default | 1–10,000 findings |

`default` removes a numeric override; unspecified fields retain engine-specific
limits. Limits are ceilings, not promises of complete analysis: Git history has
additional internal traversal, output and deadline bounds. The source coverage
notices show the effective scheduler limits. Input byte/file limits refer to the
captured PR diff, not the entire repository/history size.

Equivalent TOML (keys must be lowercase):

```toml
[repositories."github.com/owner/repo".analysis.history]
enabled = false
timeout_ms = 30000
```

Only typed built-in settings are accepted. Do not put tokens, credentials,
commands, model endpoints or other secrets in this file; unknown fields are errors.
Authentication remains managed by `gh`.

## Capabilities and result states

- Diff analysis is in-process over the already captured snapshot.
- History uses bounded local Git subprocesses. Enabling it **does not authorize
  network fetching**: that still requires `--fetch-analysis-context`. Disabling
  history skips both provider execution and history resolution/fetch.
- There is no model execution, hosted analysis, optional external tool execution,
  tool installation, or GitHub publication enabled by these preferences.
- Disabled providers remain visible as `skipped` with reason
  `disabled_by_configuration`; their finding counts are unknown, not verified zero.
  Input-budget skips, failures, and partial coverage retain their own reason codes.
- A changed effective policy supersedes only that provider's work, including its
  older in-flight attempts. Unchanged providers can reuse their results. Explicitly
  setting a value equal to its inherited limit does not invalidate results.
- Review acknowledgements, finding dispositions, exclusion rules and publication
  state are not changed by analysis policy updates. Old findings remain historical.
