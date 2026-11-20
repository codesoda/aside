---
name: aside
description: Start or join a local Aside review of a GitHub PR, read review feedback, and reply to local threads. Use when asked to review a PR with Aside or respond to feedback in an Aside session.
---

# Aside review session

Aside is a local-first PR review CLI. It opens the review immediately in a browser. The Rust server holds the authoritative state, stores rules/drafts/progress in the OS data directory, and never modifies the target repository.

## Prerequisites

- Install `aside` and GitHub CLI (`gh`). For live reviews, authenticate with `gh auth login` first.
- A full GitHub PR URL is required: `aside https://github.com/OWNER/REPO/pull/NUMBER`. For offline practice, run `aside demo --no-open`: it replays a bundled capture of https://github.com/cli/cli/pull/14921 without contacting GitHub.
- Launch in the background, keeping stdout (one JSON event per line) separate from stderr (diagnostics). Do not discard the process before the session is finished.

## Discover and attach

Read the `session.started` JSON event on stdout. It contains `sessionId`, `url`, `apiBase`, and the private descriptor path. If the same PR is already active, use the `session.attached` event instead. Never guess a port. `aside session list` and `aside session status <sessionId>` help find a session.

The descriptor is private: it contains `agentToken`, not a browser token. Never print, commit, or transmit its contents. Use the descriptor's `apiBase` and send `Authorization: Bearer <agentToken>` on API requests. Treat the token as a secret; avoid shell tracing and command logs that echo it. The health endpoint at `/api/v1/health` identifies the process; compare its `sessionId` before sending any mutation.

## Work with feedback

1. Read `GET /api/v1/session`: `pr`, `files`, `fileStates`, `threads`, `rules`, `progress`, and `sequence` describe the current review. A `fileId` comes from the session, never from an arbitrary path.
2. Follow `GET /api/v1/events` for typed SSE events. On `resync` or a sequence gap, fetch `/api/v1/session` again.
3. Investigate a human comment in your own workspace. Aside itself does not edit code. Re-fetch the session before replying if the PR changes; outdated and orphaned anchors must not be moved to a guessed line.
4. Reply with `POST /api/v1/threads/{id}/replies`, JSON `{ "body": "..." }`, authenticated with the agent token. The server persists it as an agent draft and broadcasts an event. Do not claim it was published to GitHub.

Only the human reviewer may mark files reviewed, change repository exclusions, finish/publish a review, or confirm a publication plan. Agents must never use browser credentials to bypass this boundary. `aside demo` cannot publish anything. Errors such as `stale_fingerprint` mean re-fetch state instead of retrying a stale write blindly.

`aside update --check` checks the public GitHub release redirect without an API call. `aside update` asks before replacing the executable; only an explicit `--yes` permits unattended installation. On authentication or ingestion failures, check `gh auth status`, the process stderr, descriptor permissions, and the health `sessionId`. If the server has stopped, restart Aside; never reuse a stale token or descriptor.
