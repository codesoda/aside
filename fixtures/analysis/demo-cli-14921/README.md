# demo-cli-14921 — synthetic review intelligence

Layered on the public `cli/cli#14921` demo capture (`fixtures/cli-cli-pr-14921.*`).
**Everything in this directory is synthetic**: threads, comments, reviews, checks,
statuses, runs, evidence, findings and IDs were invented for offline tests. No
record was fetched from GitHub, and no private content is included. Comment
bodies carry `(synthetic fixture)`; GitHub-style numeric IDs start at 9e9.

Layout follows `docs/plans/analyze.md` §19.1. Each JSON file deserialises into
its Rust contract type (`src/demo/intelligence.rs`) and must round-trip
byte-for-byte as JSON values; IDs must equal their derivation. Tests in
`src/demo/intelligence_tests.rs` print the expected value when a file drifts.

- `llm-hypothesis.json` is opt-in (`intelligence::load(true)`); the demo default
  has no LLM run or finding.
- `source-responses/` holds raw source-like responses (REST review comments)
  that have no contract type; they exist to test GraphQL+REST de-duplication.
- Static findings record their ID inputs under `extensions.idKey`.

`cargo test` regenerates `app/src/generated/fixtures/demoIntelligence.ts` for the
mock UI (`app/src/mock/intelligence.ts`).
