# Advanced analysis source prototype

Issue [#40](https://github.com/codesoda/aside/issues/40) evaluates three local sources. This is a hidden, experimental CLI, not a supported report API or part of a review session. Nothing is enabled by default. It does not open an Aside store, load review settings, run the project, use `gh`, or fetch Git objects.

```sh
aside prototype-sources --git-dir /path/to/repository/.git \
  --base FULL_BASE_TIP_SHA --head FULL_HEAD_SHA \
  --graph-reexports --coverage /path/to/coverage.json \
  --repository REPOSITORY_ID \
  --sarif-head /path/to/head.sarif --sarif-head-metadata /path/to/head.json \
  --sarif-base /path/to/base.sarif --sarif-base-metadata /path/to/base.json
```

Omit `--graph-reexports`, `--coverage`, or all SARIF flags to disable the respective source. Use only local inputs you trust. A checkout task can pass its actual Git directory from `git rev-parse --absolute-git-dir`; a bare repository can pass its directory directly. The prototype accepts full SHA-1 commit IDs, not refs. It reports `baseTip` separately from `comparisonBase`, the unique merge base of the supplied commits. A shallow checkout, missing object, or absent or ambiguous merge base is not a valid baseline. Graph evidence uses head only.

The `aside-advanced-sources/v0` JSON envelope reports each source separately. A disabled or unavailable source has unknown coverage, not a clean result. A completed source describes only its stated scope. A successful command means it emitted a report; check each source's state, coverage and reasons to determine what ran. Source failures do not prevent other sources from returning evidence. The Git runner caps each operation's output and shares a 30-second Git budget per source and for comparison discovery. Tree enumeration is limited to 8 MiB and 20,000 entries per commit, and the comparison patch to 32 MiB. It does not follow replacement objects or repository grafts.

Artifact paths must name regular local files. The importer does not follow a final symlink or look up paths inside an artifact on the filesystem. An actual filesystem permission denial is distinct from a missing file. No GitHub token or hosted feature support is requested or evaluated. No report or Git data is uploaded.

## Rust re-export edges

Use `--graph-reexports` to evaluate call edges that the standard structure provider does not resolve. Each edge points to the call site, the named re-export sites in order, and the function definition in the pinned head. The supported subset is one conventional Cargo root (`src/lib.rs` or `src/main.rs`) and its declared public external modules, with explicit single-item `pub use crate::…` re-exports and fully qualified `crate::…` calls. Custom targets, attributes, unsupported or ambiguous declarations, and exhausted limits cannot establish a known edge. The graph limits are 128 Rust files, 512 KiB of Rust source, 100,000 syntax nodes, 2,000 call sites, 16 re-export probes per resolution chain, and a cooperative 8-second graph budget. This is not general Rust name resolution, a cross-crate graph, or a full blast-radius report. It does not change the standard structure provider.

## Commit-pinned line coverage

Use `--coverage FILE` with a local producer report in the following format. The SHA-256 is for the exact pinned **blob bytes**, not a working-tree file after newline conversion. Paths are repository-relative. Line numbers are one-based and unique per file. The same path cannot appear twice.

```json
{
  "schema": "aside-coverage/v1",
  "commit": "FULL_HEAD_SHA",
  "tool": {"name": "example-coverage", "version": "1"},
  "files": [
    {"path": "src/lib.rs", "sha256": "BLOB_SHA256", "lines": [{"line": 10, "hits": 0}]}
  ]
}
```

Only added lines in the comparison-base → head patch can be measured. Zero hits is uncovered, positive hits is covered, and missing or unverified evidence is unknown. Unknown measurements do not count toward measured coverage. Unsupported patch shapes, including binary patches and paths requiring Git C-style quoting, do not establish coverage. The report is capped at 4 MiB, 1,024 files, 16,384 line records per file, and 4,096 added rows. Blob reads are capped at 8 MiB each and 32 MiB total. The custom format demonstrates verification; this command does not convert LCOV or run coverage tools.

## Local SARIF code scanning

Use `--sarif-head FILE` with `--repository ID` to opt in. Add `--sarif-base FILE` for the comparison base, not the base tip. The optional metadata paths name local attestations from the scan producer. A supported comparison requires matching repository identity and analysis scope, compatible scanner configurations and fingerprint semantics, and attestations of complete execution and complete export tied to the actual bytes and pinned commits. This is not independent certification of the producer or its scan completeness.

The optional sidecar format is strict JSON. Use lower-case hexadecimal SHA-256 digests for the export and exact pinned blobs. Policy fields are stable producer identifiers for the scanner, configuration, export, and suppression settings; they must agree across the pair. Declare the same set of analyzed paths and enabled rules at both revisions. The two revisions can have different contents at a path.

```json
{
  "schema_version": 1,
  "repository": "REPOSITORY_ID",
  "commit": "FULL_REVISION_SHA",
  "export_sha256": "SARIF_FILE_SHA256",
  "analyzed_paths": [{"path": "src/lib.rs", "blob_sha256": "BLOB_SHA256"}],
  "tool": {"name": "example-scanner", "version": "1"},
  "enabled_rules": ["example-rule"],
  "scanner_policy": "scan-v1",
  "config_policy": "config-v1",
  "export_policy": "full-v1",
  "suppression_policy": "none-v1",
  "fingerprint": {"key": "exampleFingerprint", "version": "1"},
  "execution_complete": true,
  "export_complete": true
}
```

A verified import accepts one SARIF 2.1.0 run with one successful invocation, exact `versionControlProvenance.revisionId`, and the same tool name and version as the sidecar. A result must name an enabled driver `ruleId`, carry the declared stable fingerprint in `fingerprints`, and identify one repository-relative location from the declared paths without an `index` or `uriBaseId`. Tool extensions and alternate `rule` or `ruleIndex` references are unsupported for classification. Without a head sidecar, a structurally supported head export can be read as raw records, but all classifications remain unknown. SARIF files and sidecars each have an 8 MiB cap. A scan can attest up to 512 paths, 10,000 rules and 10,000 results. Path attestation reads at most 256 KiB per blob and 8 MiB of blob bytes per scan.

The classifications are relative to that attested comparison. Matching stable rule/fingerprint identities are existing. A stable identity absent from a comparable complete baseline is introduced. Without strong enough evidence the status is unknown. Changed-line membership, first observation, and SARIF's own `baselineState` do not establish introduction. Artifact URIs, schema references, and external SARIF properties are never fetched or opened.

The prototype does not call GitHub code scanning, check GitHub token scopes, import remote artifacts, change alert state, create review threads, or act on scan findings.
