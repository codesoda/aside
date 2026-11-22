# Aside — Product Requirements Document

**Status:** Proposed / implementation-ready draft  
**Version:** 1.0  
**Date:** 20 November 2026  
**Product:** Aside (`aside`)  
**Type:** Local-first GitHub pull-request review application  
**Primary deliverable:** Single Rust CLI binary serving an embedded Svelte review UI

---

## 1. Executive summary

Aside is a standalone, local-first pull-request review application launched with:

```bash
aside https://github.com/owner/repository/pull/123
```

The CLI obtains pull-request data through the authenticated GitHub CLI (`gh`), constructs a typed representation of the PR and its diff, starts an Axum HTTP server on loopback, and opens a browser-based interface styled and organised like GitHub's pull-request review experience. The production frontend is a statically compiled Svelte application embedded directly in the Rust executable. No Node runtime or hosted review service is required to use the installed application.

Aside is inspired by the PR review functionality in [`codesoda/discuss-cli`](https://github.com/codesoda/discuss-cli), but it is a **dedicated PR-review product**, not another mode of a document-review application.

The defining workflow is:

> **Open PR → Review (adjust scope as you go) → Discuss findings → Finish review → Optionally publish to GitHub.**

Launching Aside drops the reviewer straight into the review. There is no separate pre-review screen or **Start review** step: saved exclusions and prior progress are applied automatically, and scope is edited in place from the review's file tree.

Two features are particularly important:

1. **Repository-wide review exclusions:** from the review's **changed-file tree**, users can ignore or re-include individual files, whole folder subtrees, and path patterns. These rules persist locally and apply automatically to future reviews of **any PR in the same repository**.
2. **Version-aware review progress:** reopening the same PR preserves which files were already reviewed. Progress is tied to the reviewed version of the **effective file diff** (alongside file content and commit identities), so unrelated new commits do not make the entire PR unreviewed, while changed files become explicitly eligible for re-review.

An `aside` agent skill documents how coding agents invoke and interact with the CLI, observe human review feedback, and reply through supported local interfaces. The Rust process—not the agent or browser—owns PR ingestion, review state, persistence, and publishing safeguards.

### 1.1 Hard requirements

- Frontend: **Svelte**, **Tailwind CSS**, **shadcn-svelte**; visually and functionally familiar to users of GitHub PR review.
- The initial frontend milestone must be **interactive and mocked**, using realistic fixture data, rather than requiring GitHub access to see or test the UI.
- Backend: **Rust CLI** with **Axum** HTTP endpoints.
- Production frontend must be a **static build embedded in and served by the Rust CLI binary**.
- `aside <pr_url>` must invoke the required `gh` commands internally, retrieve PR data/diffs, and normalise those into a typed object model served to the frontend.
- Ship an **`aside` agent skill** explaining CLI invocation and agent review workflows.
- The review's **file tree** must support adding, removing and editing file/folder exclusions, including recursive and pattern-based exclusions.
- Exclusions must **persist by repository**, without modifying `.gitignore` or the repository itself.
- Repeated reviews of the same PR must **restore review state only for matching file/diff versions** and identify files that changed since review.

### 1.2 Product principles

- **Human judgement first:** agents assist; they do not silently approve or publish reviews.
- **Private first:** locally drafted comments do not appear on GitHub until explicit publication.
- **Straight into the review:** no setup step between launch and reading diffs. Saved scope applies automatically.
- **Scope in place:** make it easy to exclude generated, vendored or noisy changes from the file tree at any point during the review.
- **Resume intelligently:** avoid re-reviewing unchanged work.
- **One local executable:** no background service or frontend runtime dependency needed in normal use.
- **Transparent state:** explain why a file is ignored, considered reviewed, or needs re-review.
- **Safe by default:** no arbitrary remote shell execution, GitHub mutations, or localhost API exposure.

---

## 2. Problem and target users

### 2.1 Problem

GitHub's PR review workflow is workable but becomes inefficient for large, noisy or repeatedly updated PRs. Reviewers frequently need to:

- Skip generated sources, snapshots, lockfiles or entire directories.
- Keep the same exclusions across many PRs in a repository.
- Return to a PR after fresh commits without losing progress on unchanged files.
- Distinguish *intentionally excluded* from *read and reviewed*.
- Collaborate with a coding agent that can investigate comments, answer questions and make changes.
- Draft privately, inspect exactly what will be published, and avoid accidental partial publication.

### 2.2 Primary users

1. **Human reviewer:** works through a PR visually, configures scope, records comments, and tracks what remains.
2. **Developer/PR author using an agent:** asks the agent to launch Aside, monitors human comments and makes/responds to fixes.
3. **Agent-assisted reviewer:** manually inspects code but asks an agent for explanations or summaries anchored to concrete files and lines.

### 2.3 Success outcomes

- A repeat reviewer does not need to reconfigure exclusions for each PR of the same repository.
- A reviewer can instantly distinguish **in scope / ignored / reviewed / changed since reviewed / not yet reviewed**.
- Reopening a PR at a later commit preserves valid prior progress.
- UI and agent clients see the same authoritative session state.
- Nothing is published to GitHub without an explicit human confirmation action.

### 2.4 Out of scope for the initial release

- A hosted SaaS review service or user account system.
- A replacement for GitHub source control, CI or merge operations.
- Automatic code changes or unattended agent approval.
- A full local Git IDE, code editor or repository browser.
- Cross-machine synchronisation of personal review progress or ignore rules.
- Multi-provider support (GitLab, Bitbucket) or GitHub Enterprise Server **in v1**.
- Editing `.gitignore` or committing review configuration to the repository by default.
- Embedding an LLM into the Rust executable.

---

## 3. User journeys

### UJ-01: First review in a repository

1. Reviewer runs `aside https://github.com/acme/widget/pull/42`.
2. CLI validates the URL and local `gh` installation/authentication, fetches metadata and diff, and starts a local UI server.
3. Browser opens directly on the **Review** screen: PR header, hierarchical changed-file tree and the diff for the first in-scope file.
4. From the file tree, reviewer ignores `src/generated/` recursively, `Cargo.lock`, and `**/*.snap`.
5. Affected files dim or move to the ignored group and leave the diff list; counts immediately update. The reviewer can inspect why a file is ignored.
6. Rules are saved against the repository identity; no manual Save step is required beyond successful persistence feedback.
7. Reviewer reads, comments, marks files viewed, and optionally finishes/publishes the review.

### UJ-02: New PR, same repository

1. Reviewer launches Aside for PR #57 in `acme/widget`.
2. Existing repository exclusion rules are loaded automatically.
3. The review opens with the rules already applied—including newly added files within ignored directories.
4. Reviewer can revise the repository rules from the file tree at any point.
5. Previously reviewed statuses from PR #42 **do not carry into PR #57**; only repository exclusions do.

### UJ-03: Resume same PR after more commits

1. Reviewer reopens PR #42 after its head commit advances.
2. Aside re-fetches the PR and computes file-level comparison identities.
3. File A has the same effective diff: **Still reviewed**.
4. File B has a changed effective diff: **Changed since reviewed** and requires re-review.
5. File C is ignored by a repository rule: **Ignored**, regardless of previous viewed state.
6. All historical comments and draft state remain available, with stale anchors identified rather than silently moved to incorrect lines.
7. Reviewer can filter to **Needs attention** and continue without repeating completed work.

### UJ-04: Exclude folder, re-include one file

1. Reviewer excludes `src/generated/**` from the review's file tree.
2. Reviewer expands the dimmed folder and selects **Include this file** for `src/generated/manual.rs`.
3. Aside persists a specific exception *after* the broader exclude rule.
4. `manual.rs` appears in scope; sibling generated files remain ignored.
5. The exception works across future PRs and can be removed from the file tree or rule manager.

### UJ-05: Human and coding agent exchange review feedback

1. Agent launches Aside or attaches to an existing session using the documented skill.
2. Human creates a local comment on a diff line.
3. The session emits a structured event to the agent.
4. Agent reads the thread and can add a clearly labelled agent reply/take.
5. Human sees the update live, can reply and resolve the discussion.
6. Any GitHub publication remains a deliberate human action.

### UJ-06: Work without GitHub or network

1. Developer runs `aside demo`.
2. Aside loads bundled, deterministic mock PR fixtures and opens the full UI.
3. Exclusions, review progress, comments, filters and update scenarios can be exercised without `gh`, authentication or network traffic.
4. Demo data is isolated from real repository rules and review history by default.

---

## 4. Functional requirements

Requirements are labelled **P0** (required for the core deliverable), **P1** (required for a usable first full release), and **P2** (follow-up enhancement). The mocked UI milestone must implement the P0 interactions with fixtures, even if the underlying GitHub and persistence integrations are completed later.

### 4.1 CLI entry point and lifecycle

| ID | Priority | Requirement |
|---|---|---|
| CLI-01 | P0 | `aside <full_github_pr_url>` is the canonical invocation. |
| CLI-02 | P0 | Parse and validate the GitHub PR URL, host, owner, repository and PR number. Reject unsupported hosts in v1 with useful errors. |
| CLI-03 | P0 | Check `gh` presence/authentication; invoke `gh` directly via argument arrays, never via a shell. |
| CLI-04 | P0 | Fetch current PR data, construct the snapshot, load local review state, and start an Axum loopback server. |
| CLI-05 | P0 | Serve embedded frontend assets, launch the default browser unless `--no-open`, and print the session URL. |
| CLI-06 | P0 | Bind `127.0.0.1` by default; use an OS-assigned port unless `--port` is supplied. |
| CLI-07 | P0 | Support `aside demo` using bundled fixtures without `gh`. |
| CLI-08 | P1 | Print a machine-readable `session.started` JSON event for agent callers, including session ID, URL and supported endpoint information. |
| CLI-09 | P1 | Detect/reuse an already-active session for the same PR, or fail clearly rather than create conflicting in-memory sessions. |
| CLI-10 | P1 | Handle Ctrl-C, browser closure and shutdown without losing persisted work. |
| CLI-11 | P2 | `aside session list`, `aside session status`, `aside session attach <id>` and related ergonomic subcommands. |

Proposed options:

```text
aside <pr_url> [--no-open] [--port <n>] [--unified <n>] [--log-level <level>]
aside demo [--no-open]
aside --version
```

The release should not require a local checkout of the target repository. Temporary Git operations may be used internally where needed for complete/accurate diffs.

### 4.2 Review file tree and scope editing

There is **no pre-review screen**. `aside <pr_url>` opens the Review screen directly with saved scope and progress applied. The review's changed-file tree is the primary place to view and edit scope, and it is always available during the review.

| ID | Priority | Requirement |
|---|---|---|
| SCP-01 | P0 | Display PR metadata, file counts, additions/deletions and hierarchical tree of changed files alongside the diff. |
| SCP-02 | P0 | Apply saved **repository-wide** ignore/include rules before drawing the tree and diff list. |
| SCP-03 | P0 | Provide per-file context-menu actions: **Ignore file**, **Include file**, and **Remove explicit rule** where applicable. |
| SCP-04 | P0 | Provide per-directory actions: **Ignore folder and descendants**, **Include folder and descendants**, and rule removal. |
| SCP-05 | P0 | Provide a pattern entry/editor (e.g. `**/*.snap`, `generated/**`) reachable from the tree. |
| SCP-06 | P0 | Support re-including an individual file under an excluded parent directory. |
| SCP-07 | P0 | Show ignored files on demand, with subdued styling, exclusion reason and matching rule. |
| SCP-08 | P0 | Update in-scope/ignored counts and the diff list immediately as rules change. |
| SCP-09 | P0 | Save changes automatically to the repository's local configuration; show success/error state. |
| SCP-10 | P0 | Launch opens the Review screen directly with scope applied; there is no Start review step. No data is deleted by filtering. |
| SCP-11 | P1 | Show resumable review states on the tree: reviewed at current diff, changed since reviewed, not reviewed. |
| SCP-12 | P1 | Provide a searchable/filterable tree and a **Manage exclusions** interface for reordering, enabling, editing and removing rules. |
| SCP-13 | P1 | Scope edits during a review never lose review progress or drafts. |
| SCP-14 | P2 | Optional per-PR override scope, visually distinct from repository defaults. |

**Expected context-menu behaviour:**

- A direct **Ignore file** action creates an explicit exclusion rule for that file path.
- **Ignore folder and descendants** creates a recursive exclusion that applies to all future paths beneath that prefix.
- **Include file** under an ignored folder creates a higher-precedence include exception; it does not remove the parent exclusion.
- **Remove rule** removes only the rule created by that action; any remaining applicable rule still determines visibility.
- A file may be affected by several rules; the UI must show the **effective rule** and permit access to the full matching-rule list.
- Disabled rules are retained but not applied.

**Recommended UI:** GitHub-style changed-files sidebar on the left, tri-state scope indicators and contextual `...` controls next to the diff. Include a clear **Show ignored** toggle and a **Needs attention** count. A folder with mixed included/ignored descendants shows an indeterminate state; acting on the folder must be explicit about recursive scope.

### 4.3 Ignore rules and matching semantics

| ID | Priority | Requirement |
|---|---|---|
| IGN-01 | P0 | Rules are stored per canonical GitHub repository identity, independent of local working directory and PR number. |
| IGN-02 | P0 | Support exact file paths, recursive directory prefixes and repo-root-relative glob patterns. |
| IGN-03 | P0 | Support explicit **exclude** and **include** actions with deterministic precedence. |
| IGN-04 | P0 | Include/exclude rules are evaluated over candidate changed file paths; excluded parent directories must not block a later child include exception. |
| IGN-05 | P0 | New files under an excluded directory inherit the exclusion automatically. |
| IGN-06 | P0 | Changing rules must never modify GitHub, `.gitignore` or the checked-out repository. |
| IGN-07 | P0 | Rules persist across sessions and across different PRs in the same repo. |
| IGN-08 | P0 | Ignoring a file must **not** mark it reviewed, remove historical review records or delete comments. |
| IGN-09 | P1 | Show why any file is ignored or included and provide a way to reverse the effective rule. |
| IGN-10 | P1 | Renamed paths are evaluated under their new PR path; preserve historical path linkage separately for review history. |
| IGN-11 | P2 | Export/import repository rule sets and optionally share them through an explicit project config file. |

**Rule evaluator decision:** support Gitignore-*inspired* syntax for common globs but implement **explicit ordered last-match-wins include/exclude semantics on flattened PR file paths**. Do **not** blindly inherit Git's parent-directory re-inclusion restrictions: the requirement to re-include a child beneath an excluded folder is intentional. Define slash anchoring, `**`, `*`, `?`, escaping and case sensitivity in an implementer-facing parser specification and tests. Paths use repository-relative `/` separators and the platform-independent path representation supplied by GitHub. Treat pattern validation errors as editable UI errors; do not silently accept invalid rules.

**Default scope:** repository-wide. A possible later PR-only override must be an opt-in, separately indicated scope; the UI must never silently save a PR-only change as a global rule or vice versa.

### 4.4 Diff display and review interaction

| ID | Priority | Requirement |
|---|---|---|
| REV-01 | P0 | Present a GitHub-like PR review surface with a changed-file tree and diff viewer. |
| REV-02 | P0 | Render per-file headers, addition/deletion counts, syntax-highlighted hunks, context/addition/deletion lines and old/new line numbers. |
| REV-03 | P0 | Support file navigation, expand/collapse and switching between all files, in-scope files and needs-attention files. |
| REV-04 | P0 | Mark each in-scope file **Reviewed/Viewed** from the diff interface. |
| REV-05 | P0 | Preserve a clear distinction among ignored, never-reviewed, reviewed-at-current-diff and changed-since-reviewed. |
| REV-06 | P0 | Create local line-anchored comments and threaded replies using a realistic mocked UI and eventually the Axum API. |
| REV-07 | P1 | Support unified and side-by-side/split diff modes. |
| REV-08 | P1 | Display existing GitHub reviews, review threads, issue comments and review summaries when retrievable. |
| REV-09 | P1 | Support thread resolution, navigation and a summary of open discussions. |
| REV-10 | P1 | Offer **Finish review** with Approve, Request changes or Comment-only and an editable summary. |
| REV-11 | P1 | Before publishing, show an exact text-first preview of outbound review operations and require explicit confirmation. |
| REV-12 | P1 | Refetch/check head SHA and anchors before publishing; preserve drafts and report stale/unpublishable items. |
| REV-13 | P2 | Expand unchanged context and navigate to source context or related files. |
| REV-14 | P2 | Add keyboard shortcuts and advanced review queue controls. |

Ignored files remain inspectable behind **Show ignored**. Any deliberate inspection does not itself change exclusion rules or review status. Review progress calculations exclude ignored files from the *required* denominator while reporting them separately.

### 4.5 Digest-aware persistence and repeated reviews

| ID | Priority | Requirement |
|---|---|---|
| PER-01 | P0 | Persist reviewed status for **each file version within each PR**, not as a global boolean by path. |
| PER-02 | P0 | Opening the same PR restores prior review records when the current effective file diff fingerprint matches. |
| PER-03 | P0 | Changing only unrelated files/head commits does not invalidate a matching file review. |
| PER-04 | P0 | When a file's effective diff changes, mark it **Changed since reviewed** and require an explicit new review acknowledgement. |
| PER-05 | P0 | Store PR head commit SHA, comparison-base identity, current file content digest (where available) and canonical per-file diff digest. |
| PER-06 | P0 | Persist ignore rules at repository scope, while review marks/comments/drafts remain PR-scoped. |
| PER-07 | P1 | Preserve a history of review acknowledgements (timestamp, actor, reviewed fingerprint), rather than overwriting old records. |
| PER-08 | P1 | Persist local comments, replies, draft publication state and review session metadata. |
| PER-09 | P1 | Flag stale/orphaned discussion anchors after a PR update, without guessing and reposting to the wrong line. |
| PER-10 | P1 | Provide a manual **Review again** action that marks the current fingerprint reviewed, preserving history. |
| PER-11 | P2 | Allow inspection of review history across multiple PR revisions. |

**Fingerprint design:**

- **`head_sha`** identifies the current PR head revision, useful for freshness checks and GitHub publication safety.
- **`content_digest`** identifies the resulting file contents (Git blob SHA or independently computed digest), where obtainable.
- **`diff_digest`** identifies the review-relevant change for that file against the PR's comparison base.
- The **review fingerprint** must be defined as a versioned hash of a canonical representation of the relevant per-file patch, including path/rename identity, change type, modes and changed/context lines as appropriate. It should not include incidental transport headers, fetch timestamps, or unrelated PR head changes.
- Store `fingerprint_version` alongside the hash so a future algorithm change can safely invalidate or migrate old marks.
- Do not assume a content digest alone proves the same change was reviewed: comparison base and diff semantics can shift.
- A new PR commit that leaves the effective per-file patch unchanged should retain the reviewed state. A materially changed patch invalidates it, even if the filename is the same.
- Differences solely in incidental hunk offsets may be normalised **only if anchors and review-relevant semantics remain equivalent**. Changes to surrounding context that affect interpretation should not be carelessly normalised away.
- Initial implementation may err on the side of **re-review** for ambiguous equivalence, never on the side of falsely retaining a reviewed mark.

**Status is derived, not simply stored:**

```text
ignored                 if active repository/PR rules exclude current path
reviewed-current        if a prior acknowledgement matches current fingerprint
changed-since-reviewed  if a prior acknowledgement exists for this PR file/path lineage,
                        but not for current fingerprint
unreviewed              otherwise
```

A previously reviewed file that becomes ignored still retains its review history. Re-including it reveals its current derived review status. Deleted files and renames need stable path lineage/reference records so they do not silently inherit status from unrelated paths.

### 4.6 Review completion and GitHub publication

**P1 functional release** (not a dependency for the first interactive mock):

- All comments are local drafts by default.
- The reviewer can choose Approve, Request changes or Comment-only.
- Reviewer can individually select draft threads/replies for inclusion in the outbound review.
- The app constructs a precise, human-readable publication plan, including targets, texts and omissions.
- The user must explicitly confirm **Publish to GitHub** after seeing this plan.
- Rust revalidates PR head, line anchors and publication permissions before executing `gh` operations.
- Invalid/outdated anchors remain local with reasons; never publish them to a guessed destination.
- Publishing must be idempotent or safely recoverable; partial failures are surfaced honestly, and any remaining drafts are preserved.
- Review completion and review publication are distinct states: a session may be finished locally but not published.
- A previously published review does not prevent reopening a PR to inspect a later diff, though publishing a new review requires a new explicit confirmation.

### 4.7 Agent skill and agent-facing behaviour

| ID | Priority | Requirement |
|---|---|---|
| AGT-01 | P0 | Provide `skills/aside/SKILL.md` explaining what Aside is, when to invoke it, `aside <pr_url>`, prerequisites and available workflows. |
| AGT-02 | P0 | Skill describes how to launch a review in the background without confusing stdout events and logs. |
| AGT-03 | P1 | Expose a documented agent interface to read session state and respond to local threads. |
| AGT-04 | P1 | Publish structured session/thread events for monitoring and support SSE for browser updates. |
| AGT-05 | P1 | Agent messages are distinguished visually and in persistence from human-authored messages. |
| AGT-06 | P1 | Agent can respond to review feedback but cannot silently publish or modify repository exclusions by default. |
| AGT-07 | P1 | Agent skill must work with Claude Code, Codex and compatible skill loaders. |
| AGT-08 | P2 | Add convenient CLI wrappers for common agent actions instead of requiring raw HTTP calls. |

Recommended skill content:

1. Preconditions: `aside`, `gh`, authenticated GitHub session.
2. Start/attach and monitor session via machine-readable events.
3. Discover the actual local API URL and credentials from the session descriptor; **never guess ports**.
4. Read current PR state, active scope, open human threads and current reviewed fingerprints.
5. Investigate feedback in the agent's own repository workspace (Aside does not automatically edit code).
6. Post an agent reply/take against a specific thread and fingerprint/anchor; acknowledge stale states.
7. Explain completion and human-only GitHub publication boundaries.
8. Troubleshooting, authentication failures and session restoration.

**No custom MCP server is required for v1.** The skill can use the CLI's documented session events and local HTTP interface. Agent-friendly CLI subcommands may be added to reduce brittle ad-hoc `curl` invocations.

---

## 5. Information architecture and UI specification

### 5.1 Screens

1. **Review** — initial screen on launch; PR header, left changed-file tree with include/exclude controls and review-state indicators, main diff viewer, inline comments and navigation/actions.
2. **Finish review** — verdict, editable summary, selected drafts and exact publication preview.
3. **Manage exclusions** — entry from the review file tree; list/edit/reorder rules and inspect matches (may be a drawer/dialog rather than a route).
4. **Demo** — same surfaces populated from fixtures and labelled synthetic data.

### 5.2 Layout details

**PR header:** repository, PR number/title, author, branch names, base/head SHAs (short), changed-file count, review progress and GitHub link.

**Review left panel:** expandable directory tree with file icons, changed-file count badges, review-state badges, three-dot context menu and effective exclusion indication. Excluded files should remain discoverable. A folder with mixed exclusions uses an indeterminate indicator. Quick filters, the glob editor (with live matching preview) and **Manage exclusions** are reached from this panel.

**Remembered state:** when saved exclusions or prior progress apply, show a short, dismissible notice above the diff (for example, "12 files ignored by repository rules · 5 already reviewed") instead of a separate screen.

**Diff panel:** GitHub-inspired typography/spacing, line gutters, syntax highlighting, changed-line backgrounds, comment targets, file headers, viewed control, thread stacks, unified/split toggle and next-needs-attention navigation.

**Status vocabulary:**

| Status | Meaning | Presentation |
|---|---|---|
| Unreviewed | Current diff has never been acknowledged | Neutral |
| Reviewed | Current diff fingerprint was acknowledged | Viewed/check indicator |
| Changed since reviewed | Previous acknowledgement exists, current fingerprint differs | Distinct warning/refresh indicator |
| Ignored | Explicit or pattern rule excludes this path | Dimmed/eye-off, with explanation |
| Stale discussion | Thread refers to outdated/invalid anchor | Warning with manual re-anchor flow |

**UI accessibility:** keyboard-navigable tree and menus, focus indicators, labelled screen-reader actions, contrast-safe light/dark themes, and non-colour cues for every status. Responsive behaviour may optimise for desktop-width viewports; narrow screens must not silently lose review actions.

### 5.3 Mock-first frontend fixtures

The first deliverable is a running Svelte UI with realistic mocked data and active interactions. Fixtures should cover:

- Normal medium-sized PR across Rust, TypeScript and config files.
- Large PR with nested generated directory and snapshot files.
- Existing local and imported threads.
- An already-reviewed file with unchanged digest.
- A previously reviewed file with a changed digest.
- A recursive exclusion plus explicit child re-inclusion.
- A rename, deletion, binary file and mode-only change.
- A PR refresh that triggers stale anchors.
- An empty-in-scope review after all files are excluded.

**Mock contract rule:** fixture objects must conform to the eventual real Rust/TypeScript API schema. Avoid creating a visually polished mock whose data shape cannot be produced by the backend.

---

## 6. Technical architecture

### 6.1 Components

```text
aside <pr_url>
    │
    ▼
Rust CLI process
    ├── GitHubAdapter (invokes authenticated `gh`)
    │     ├── PR metadata / files / review threads
    │     └── aggregate diff (and robust fallback when needed)
    ├── Diff parser + canonical fingerprint engine
    ├── Review domain / rule evaluator / session service
    ├── SQLite persistence
    ├── Axum HTTP API + SSE event bus
    └── Embedded Svelte static assets
            ▲
            │ same-origin REST / SSE
            ▼
       Browser review UI

Agent process
    ├── `aside` skill + session stdout events
    └── authenticated local API or supported CLI subcommands
```

### 6.2 Frontend stack

- **Svelte** (prefer current supported Svelte version), **Vite** for frontend development/build, **TypeScript** for typed API consumers.
- **Tailwind CSS** for utilities and design tokens.
- **shadcn-svelte** for accessible primitives (menus, dialog/drawer, buttons, inputs, tooltips, tabs, dropdowns), styled to be recognisably GitHub-inspired.
- A suitable syntax highlighter supporting relevant programming languages, with diff semantics (additions/deletions) rendered separately from language tokens.
- A thin API client that can switch between a deterministic mock adapter and live REST endpoints without changing component logic.
- No runtime dependency on a Svelte development server, external CDN, third-party JS host or internet connection for core assets.

**Development:** run Vite with hot reload and proxy `/api` to a local Rust Axum process, or use a configurable same-origin-equivalent dev proxy.  
**Production:** `vite build` outputs immutable assets (including `index.html`) into a dist directory; embed those bytes at Rust compile time (`rust-embed`, `include_dir`, or an equivalent tested approach). Axum serves appropriate MIME types, SPA fallback, cache headers and ETags. Only frontend routes fall back to `index.html`; unknown `/api/*` routes return JSON 404.

### 6.3 GitHub ingestion

The Rust backend owns GitHub access and executes the user's existing `gh` installation. At a minimum:

- Resolve and validate PR identity and repository metadata.
- Fetch PR title, body, author, branches, base/head SHAs, review status and changed-file metadata.
- Fetch the patch/diff with enough context for comments and GitHub-compatible line anchors.
- Fetch existing review summaries, inline threads and comments when available; use `gh api` for API capabilities not provided by `gh pr view --json`.
- Parse the aggregate diff into **files → hunks → lines**, retaining old/new positions, file operations and source patch positions.
- Record GitHub review-thread IDs, original paths, commit identities and outdated/original line coordinates necessary for safe reply/publication workflows.

Use an explicit, testable ingestion strategy rather than assuming `gh pr diff` is complete for every PR. For large/truncated/unavailable patches, evaluate a temporary filtered clone and immutable PR refs, as `discuss-cli` does. Do not mutate a user's working tree. Provide meaningful errors for inaccessible repos, auth failures, rate limiting, binary-only changes, empty diffs and oversized PRs.

**Important:** GitHub's patch position, modern `line`/`side` review anchor fields, and old/new line numbers are *not interchangeable*. The domain model must carry enough provenance to map accurately to GitHub's supported publication parameters.

### 6.4 Data transfer: REST snapshot + SSE

**Decision: use a typed REST snapshot for initial state, and Server-Sent Events for subsequent updates.**

- Browser loads `GET /api/v1/session` or `GET /api/v1/pr` after the static app starts.
- Server serialises its authoritative Rust models to JSON through Serde.
- Svelte subscribes to `GET /api/v1/events` for file state, rule changes, comments, agent replies and PR refreshes.
- Browser sends mutations via authenticated same-origin HTTP endpoints; Rust persists and then broadcasts events.
- Event envelopes carry `schemaVersion`, `sessionId`, monotonically increasing `sequence`, `eventType`, timestamp and payload.
- On reconnect or sequence gap, client re-fetches the authoritative snapshot rather than assuming delivery is complete.
- Use generated TypeScript declarations from Rust types (e.g. `ts-rs`/`specta`) or a generated OpenAPI contract to avoid drift.

**Why not bootstrap JSON in HTML?** Embedding live PR content into the HTML template would couple data to asset caching and increase XSS/correctness risk. A separate JSON snapshot keeps the static frontend truly static and improves mock/test isolation.

**Why SSE rather than WebSockets?** The app mainly needs server-to-client notifications, while mutations fit REST. SSE is simpler for a one-process local app. Use WebSockets only if later proven necessary.

### 6.5 Proposed API surface

These are **proposed routes**, to be finalised as a versioned contract:

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/session` | Initial PR snapshot + session + effective review state |
| GET | `/api/v1/events` | SSE for subsequent changes |
| GET | `/api/v1/repositories/current/exclusions` | Repository rule list and effective matches |
| POST | `/api/v1/repositories/current/exclusions` | Create include/exclude rule |
| PATCH | `/api/v1/repositories/current/exclusions/{id}` | Edit/toggle/order existing rule |
| DELETE | `/api/v1/repositories/current/exclusions/{id}` | Remove rule |
| POST | `/api/v1/files/{id}/reviewed` | Acknowledge current file fingerprint |
| GET | `/api/v1/files/{id}/history` | Previous acknowledgements and fingerprints |
| POST | `/api/v1/threads` | Create local anchored review comment |
| POST | `/api/v1/threads/{id}/replies` | Reply as human/agent |
| PATCH | `/api/v1/threads/{id}` | Edit/resolve local thread subject to permissions |
| POST | `/api/v1/refresh` | Re-fetch PR/diff and rederive state |
| POST | `/api/v1/review/finish` | Store local verdict/draft summary |
| POST | `/api/v1/review/publish/selection` | Human-only atomic local draft selection (`commentIds`, `selected`; 1–500 IDs) |
| POST | `/api/v1/review/publish/preview` | Human-only exact planned GitHub changes, omitted drafts and blockers; available offline in demo |
| POST | `/api/v1/review/publish` | Apply explicitly confirmed, unblocked plan after freshness and selection-revision checks; forbidden in demo |

All mutation endpoints must return the effective updated state or sufficient event/sequence metadata for deterministic UI reconciliation. Use stable IDs unrelated to tree display order; map file IDs to a validated session path, not arbitrary filesystem access. Stale snapshot/fingerprint writes should return `409 Conflict` with a machine-readable reason and fresh data.

### 6.6 Domain model (illustrative, not code-complete)

```text
RepositoryIdentity
  host, provider_repository_id?, owner, name

PullRequestIdentity
  repository_id, number

PRSnapshot
  schema_version, pr_identity, title, body, author, url
  base_ref, base_sha, head_ref, head_sha, fetched_at
  files[], imported_threads[], warnings[]

ChangedFile
  id, path, previous_path?, change_kind, mode_changes?
  additions, deletions, is_binary, file_blob_sha?
  diff_digest, fingerprint_version, hunks[]

DiffHunk
  id, old_start, old_count, new_start, new_count, header, lines[]

DiffLine
  id, kind(context|addition|deletion|meta)
  text, old_line?, new_line?, patch_position?, side?

ExclusionRule
  id, repository_id, action(include|exclude), kind(file|directory|glob)
  expression, enabled, order, created_at, updated_at

ReviewAcknowledgement
  id, pr_identity, file_identity/path_lineage
  path_at_review, fingerprint_version, diff_digest
  content_digest?, head_sha, base_sha, actor, reviewed_at

ReviewSession
  id, pr_identity, current_snapshot_id, lifecycle_state
  local_drafts[], thread_state[], verdict?, last_opened_at

ReviewThread
  id, source(local|github), github_thread_id?, file_identity
  anchor(current|outdated|orphaned), comments[], resolved
```

Separate immutable-ish `PRSnapshot` and mutable `ReviewSession`/`RepositoryRules`. The effective tree and file status are **derived views** combining these three sources, not independently stored booleans in the frontend.

### 6.7 Persistence

Use SQLite for local durable state, with migrations and transactional updates. Resolve an OS-appropriate app data location (XDG config/data conventions on Linux, Application Support on macOS, app data conventions on Windows). No `.aside` directory or database is written into a repository by default.

Suggested logical tables:

- `repositories` — canonical host/owner/name plus provider ID when known.
- `repository_exclusion_rules` — ordered include/exclude rules and metadata.
- `pull_requests` — stable PR identity and latest-known metadata.
- `pr_snapshots` / `snapshot_files` — fetched revisions and file fingerprints (retention policy configurable).
- `review_acknowledgements` — history of reviewed versions, actor and timestamp.
- `review_sessions` — open/closed/reopened state and UI restoration metadata.
- `review_threads`, `review_comments`, `review_drafts` — local and imported discussions.
- `publication_attempts` — previews, confirmations, outcomes and partial failures.

Repository ID should remain stable across PRs. Prefer the provider's immutable repository ID if obtainable, with canonical URL/name as a fallback; handle renamed repositories through alias reconciliation rather than creating duplicate rules.

Use durable transactions before reporting successful rule changes and reviewed marks. Concurrency safety matters when two browser tabs or agent processes address one session. Version and migration failures should be recoverable; corrupted or unavailable local persistence must never produce false reviewed states.

### 6.8 Security and trust boundaries

- Bind HTTP to loopback by default; do not expose a network listener accidentally.
- No permissive CORS. Validate `Origin`/`Host`; protect write routes against cross-origin requests and CSRF.
- Use session-specific, unguessable credentials/cookies for UI mutations and a controlled local credential/descriptor for agent clients; do not embed privileged agent tokens in public JS bundles or logs.
- Store local session descriptors/credentials with restrictive filesystem permissions where supported.
- Invoke `gh` with structured argv; do not construct shell command strings from PR paths, comments or filenames.
- Escape untrusted PR titles, Markdown and diff text; avoid injecting raw GitHub HTML into the Svelte DOM.
- Guard against path traversal, huge/binary payloads, malicious filenames and resource exhaustion in diff parsing.
- Redact tokens and sensitive data from stdout, logs and error responses.
- Separate read-only browsing, local mutations and GitHub publishing permissions at the application layer.
- Any preview/publish action checks a fresh head SHA and valid line/thread anchors.
- `aside demo` must be incapable of invoking real GitHub publication.

---

## 7. State, rules, and edge cases

### 7.1 Rule precedence examples

Assume ordered rules evaluated over *individual changed-file paths*:

```text
1  exclude directory  src/generated/
2  exclude glob       **/*.snap
3  include file       src/generated/manual.rs
```

Results:

| Path | Effective state | Why |
|---|---|---|
| `src/generated/types.rs` | Ignored | Rule 1 |
| `src/generated/manual.rs` | Included | Rule 3 overrides rule 1 |
| `tests/output.snap` | Ignored | Rule 2 |
| `src/api.rs` | Included | No matching exclusion |

A rule editor should display effective match counts and warn when a newly added rule shadows another. Folder context menus must be implemented using this same evaluator, not custom view-only flags.

### 7.2 Review version examples

| Current file | Previously reviewed | Outcome |
|---|---|---|
| `src/a.rs` same effective patch | Same `diff_digest` | Keep reviewed |
| `src/b.rs` changed patch | Old digest only | Changed since reviewed |
| `src/c.rs` new in PR | None | Unreviewed |
| `generated/schema.rs` current patch changed | Excluded by repo rule | Ignored; historical review preserved |
| `src/a.rs` same patch, PR head changed elsewhere | Same digest | Keep reviewed |
| `src/renamed.rs` renamed from another path | Prior path lineage | Only reuse if identity/equivalence rules safely prove match; otherwise re-review |

### 7.3 Edge-case requirements

- **All files ignored:** the Review screen still opens, but presents an explicit message that no files are in scope; do not imply all code was reviewed.
- **Empty PR / no textual diff:** show metadata and explanatory empty state.
- **Binary or mode-only file:** show file entry and non-commentable metadata; allow scoped acknowledgement if supported, but not invented inline anchors.
- **File deleted:** show deletions, with valid old-side anchors if publication supports them.
- **Rename:** show old/new paths, match current exclusions on current path, and retain previous path for identity and comments.
- **Forced push/base branch changes:** recompute fingerprints from the new snapshot, flag stale anchors, preserve local drafts/history.
- **Network/auth failure on refresh:** retain last known snapshot as *stale/read-only where appropriate*, show clear error, never silently claim fresh data.
- **GitHub rate limit/large PR:** report partial/incomplete ingestion; block actions requiring a complete patch.
- **Simultaneous UI and agent actions:** apply transactional writes and revision checks; broadcast canonical updates.
- **Two repositories with the same name:** never share rules unless canonical identity matches.
- **Untrusted path string:** normalise separators and prevent escape outside repository path namespace.
- **Long-lived session:** reconnect SSE and reconcile state after network/tab sleep.
- **File ignored then re-included:** prior review and local comments are still available; current fingerprint determines progress.
- **Rule removal affects multiple files:** UI previews count changes and updates the tree immediately after successful write.
- **Publishing an outdated thread:** preserve and explain the failure rather than publishing to an incorrect line.

---

## 8. Delivery plan

### Milestone 0 — repository and contracts

**Goal:** establish the project boundary and data contracts.

- Scaffold Rust workspace/CLI/Axum app and Svelte/Vite/Tailwind/shadcn-svelte frontend.
- Define domain types for PR snapshot, changed files, hunks, lines, exclusion rules, review acknowledgements and threads.
- Set up Rust → TypeScript type generation and fixture validation.
- Establish packaging/build automation and a demo-only route.

**Exit criteria:** frontend build starts locally; Rust serves a placeholder/static shell; mock fixture and Rust contract agree.

### Milestone 1 — interactive mocked review UI (**first visible deliverable**)

**Goal:** fully interactable GitHub-style review UX without GitHub connectivity.

- PR header, file tree, context menus and central review surface.
- File, folder and glob exclusions with ordered include exceptions.
- Scope counts, reason indicators, toggle ignored, resume state applied on launch.
- Reviewed/changed/unreviewed status fixture scenarios.
- Diff navigation, syntax highlighting, comment drafts, thread replies and review controls.
- Mock adapter and deterministic refresh scenarios.

**Exit criteria:** all scope and progress journeys can be demonstrated in the browser using fixture data. No nonfunctional decorative controls disguised as working actions.

### Milestone 2 — Rust ingestion and live API

**Goal:** replace fixture data with real PR snapshots.

- `gh` auth checks, PR metadata/reviews/diff collection.
- Robust parser and canonical per-file fingerprints.
- Axum typed JSON snapshot and SSE events.
- Svelte live API adapter with fixture adapter retained for testing/demo.
- Embedded static asset delivery in the Rust binary.

**Exit criteria:** `aside <pr_url>` opens a real PR review in the browser with correctly parsed files/hunks/lines.

### Milestone 3 — durable exclusions and review resumption

**Goal:** make Aside genuinely useful across sessions and PRs.

- SQLite migrations and repository identity handling.
- Repository-level exclusion CRUD from the review file tree.
- Ordered include/exclude semantics and live recalculation.
- File-version acknowledgements with diff fingerprint history.
- PR reopen, refreshed-head comparison, stale indicator logic and concurrency checks.

**Exit criteria:** a rule created on PR #1 applies to PR #2 in the same repo; reopening PR #1 preserves only valid review acknowledgements.

### Milestone 4 — agent and discussion workflow

**Goal:** connect human reviews with an agent through a stable local contract.

- Persisted local threads/replies and imported GitHub discussion state.
- SSE and machine-readable stdout events.
- Agent-safe local session descriptor/credentials and API documentation.
- `skills/aside/SKILL.md` and install support for common agent skill roots.
- Agent takes visually distinguishable from human comments.

**Exit criteria:** a coding agent can launch/attach, receive a human comment and post a labelled reply without GitHub publication.

### Milestone 5 — publishing, packaging and release

**Goal:** production-grade, private-first PR review.

- Finish Review verdict and editable summary.
- Exact outbound publication preview and explicit confirmation.
- Head/anchor revalidation, idempotent/recoverable publishing and failure recovery.
- Cross-platform CLI builds, embedded frontend asset verification, installer and documentation.
- Security tests and end-to-end acceptance suite.

**Exit criteria:** a human can review a real PR, safely publish selected feedback, and restart/reopen the app without losing repository exclusions or review history.

---

## 9. Acceptance criteria (testable)

### 9.1 Product behaviour

1. **CLI launch:** with authenticated `gh`, `aside <pr_url>` opens a working localhost UI showing the correct PR; `--no-open` does not launch a browser.
2. **Offline demo:** `aside demo` exercises mocked review workflows without `gh` or network calls.
3. **Recursive exclusion:** excluding `vendor/` from the review file tree hides/excludes all current descendant files and future descendants in a later PR.
4. **File exception:** after excluding `vendor/`, including `vendor/README.md` leaves only that file in scope within the subtree.
5. **Pattern exclusion:** entering `**/*.snap` excludes matching nested snapshot files; the UI shows the matching rule and affected count.
6. **Rule reversal:** deleting or disabling a rule immediately restores affected files to their derived scope, subject to other rules.
7. **Repository persistence:** exclusions configured for `acme/widget` PR #42 are present on PR #57 after app restart, but not on unrelated `acme/another-repo`.
8. **No repository mutation:** ignore changes do not modify GitHub, `.gitignore` or working-tree files.
9. **Review acknowledgement:** marking a file reviewed stores its current review fingerprint and persists across restart.
10. **Unrelated commit:** a changed PR head with an unchanged file patch leaves that file marked reviewed.
11. **Changed file:** a changed patch for a previously reviewed file is labelled *Changed since reviewed*; it is not treated as currently reviewed.
12. **Ignore precedence:** ignoring/re-including never deletes a file's historical review acknowledgement or comments.
13. **Progress counts:** in-scope reviewed/remaining, changed-since-reviewed, and ignored are separately counted, with no double-counting.
14. **Diff fidelity:** line numbers, change type, renames, deleted files, binary files and file status are correctly represented by the typed JSON model.
15. **Comment anchors:** new comments are attached to stable file/line identities; stale anchors are detected after refresh.
16. **Live sync:** browser and agent see persisted changes through snapshot + events; reconnect recovers from missed events.
17. **Private-first:** posting a local comment never writes to GitHub before human publication confirmation.
18. **Safe publish:** publication preview shows exact selected destinations and contents; changed head or invalid anchor prevents unsafe publication.
19. **Single binary:** an installed Aside executable can serve its production UI without Node/npm or frontend files installed beside it.
20. **Agent skill:** a skill-compatible agent can follow `SKILL.md` to start a session, discover endpoints, read comments, and reply using documented authenticated methods.

### 9.2 Automated testing strategy

- **Unit tests:** ordered path matching (especially excluded-parent/included-child), normalisation, canonical diff fingerprints, state derivation, safe URL parsing and publication revalidation.
- **Fixture/golden tests:** multi-file patches, renames, deletions, binary changes, CRLF/LF, no-newline markers, mixed contexts, stale anchors and GitHub API response variation.
- **Persistence tests:** migration upgrades, same repo/new PR, same PR/new head, re-include after ignore, simultaneous writes and process restart.
- **Frontend component tests:** tree context menus, mixed folder states, pattern preview, counts, filters, reviewed status and inline threads.
- **End-to-end tests:** bundled mock (`aside demo`) with browser automation; real GitHub interaction tests against test repos/fixtures under explicit opt-in.
- **Release smoke tests:** executable contains HTML/CSS/JS assets and serves them on macOS/Linux/Windows target builds.
- **Security tests:** CSRF/cross-origin requests, local credentials, untrusted diff strings, path traversal and stale publication attempts.

---

## 10. Non-functional requirements

- **Local-first:** all essential UI assets and review persistence live on the user's machine. Network is required only for real GitHub access and explicit GitHub publication.
- **Reliability:** a browser reload or process restart must not lose acknowledged review state or saved exclusion rules; writes are committed transactionally.
- **Performance:** the app should feel responsive for ordinary PRs; lazy render/virtualise large trees and diffs, and impose configurable input size limits with visible warnings rather than freezing.
- **Observability:** structured logs identify ingestion, persistence, refresh and publish failures without logging GitHub credentials or full private PR content by default.
- **Portability:** target macOS and Linux first, with Windows packaging supported by an explicit release matrix; exact CPU architectures to be selected in CI.
- **Data compatibility:** version API events, persisted fingerprints and SQLite migrations to support safe upgrades.
- **Accessibility:** keyboard use, focus control, readable contrast, labelled status icons and responsive layouts.
- **Safety:** fail closed on ambiguous line anchors and incomplete diff fetching.

Suggested **initial engineering targets**, to be validated rather than treated as established facts:

- No dependency on Node/npm to use a packaged release.
- Typical mock PR review surface interactive shortly after app load.
- Large PR handling avoids unbounded synchronous main-thread rendering.
- Repository exclusion writes confirmed after durable storage succeeds.
- No PR publication occurs during demo or while only browsing/reviewing locally.

---

## 11. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| GitHub patch/anchor API differences | Comments attach incorrectly or cannot publish | Preserve all coordinate systems and commit provenance; fixture tests; refuse ambiguous publication |
| Fingerprint algorithm falsely preserves a reviewed mark | Reviewer misses changed code | Conservative versioned canonical fingerprints; re-review on ambiguity; regression tests |
| Rules overlap unexpectedly | Files are unintentionally skipped | Ordered last-match-wins evaluator; effective-rule explanations; explicit child exceptions and previews |
| PR head changes mid-session | Stale comments or reviews | Refresh/revalidate head and anchors immediately before publish; block stale mutations |
| Large PR overwhelms UI or parser | Slow/hung app | Size bounds, streaming/async ingestion, virtualised rendering and honest partial state |
| Agent and browser race on state | Lost replies or inconsistent status | Server-authoritative state, transactional persistence, revision checks, SSE reconciliation |
| Localhost API abused by another webpage | Unauthorised local changes | Origin/Host checking, strong session credentials, CSRF defence and loopback binding |
| Mock UI drifts from real domain model | Expensive integration rewrite | Shared generated types and contract-validated fixtures from day one |
| Repository renamed or transferred | Exclusions seem lost | Prefer immutable provider repository identity and reconcile aliases |
| CLI binary embeds stale/missing assets | Broken release UX | Reproducible build ordering and asset-serving smoke tests |

---

## 12. Decisions and remaining design questions

### Agreed requirements

- Name: **Aside**; CLI invocation `aside <pr_url>`.
- GitHub-style browser PR review interface.
- Svelte + Tailwind + shadcn-svelte frontend.
- Rust CLI + Axum server.
- Production static frontend embedded into the CLI executable.
- `gh` handles GitHub access; Rust parses its output into a PR object model.
- Agent-facing `aside` skill.
- First frontend implementation is interactive and fixture/mock driven.
- Launch drops straight into the review; there is no pre-review screen or Start review step.
- The review file tree is the primary place to modify exclusions.
- Exclude individual files, whole directory subtrees, and glob patterns; allow reversal/exception.
- Exclusions persist **per repository** across future PRs.
- Reviewed state persists **per PR and per file/diff version** across repeated reviews.

### Proposed defaults requiring validation during implementation

| Decision | Proposed default | Why |
|---|---|---|
| Initial browser data | REST snapshot + SSE updates | Simple, typed, live and mockable |
| Persistence | Local SQLite | Durable rules, sessions and review history |
| Ignore precedence | Ordered last-match-wins includes/excludes | Supports child inclusion beneath excluded folder |
| Rule scope | Repository by default | Explicit user requirement |
| Review identity | Versioned canonical per-file diff digest, with content and SHA provenance | Avoids resetting unrelated unchanged files |
| Diff acquisition | `gh` metadata + patch; temporary clone fallback for fidelity | Handles GitHub API/patch limitations |
| Server bind | `127.0.0.1`, OS-assigned port | Local-first, avoids collisions |
| Concurrent same-PR sessions | Reuse one active session where possible | Avoid state divergence |
| Published review | Requires separate preview and confirmation | Private-first and safe |
| Cross-provider support | GitHub.com only in v1 | Contained first release |

### Open questions, **not blockers** to Milestone 1

1. Exact canonical fingerprint normalisation and treatment of context-only/base-shift changes; must be designed and tested conservatively.
2. Whether the eventual v1 should include PR-only ignore overrides or keep repository-only rules; repository-wide is mandatory.
3. Whether a file acknowledgement means **Viewed** (GitHub parity) or a stronger **Reviewed** assertion; UI should clearly distinguish terminology and semantics.
4. Final agent client ergonomic interface: raw local API, extra CLI subcommands, or both; the agent skill must document the chosen stable interface.
5. Automatic PR-refresh policy versus explicit refresh; updates must not silently clear comments or acknowledgements.
6. Cross-platform release targets, installation method and auto-update behaviour.
7. Whether split diff is P1 or P0 after visual prototype testing.

---

## 13. Definition of done

Aside v1 is complete when a reviewer can install **one executable**, run `aside <real_pr_url>`, see a GitHub-familiar PR interface, set and edit repository-wide file/folder/pattern exclusions **directly from the review file tree**, perform and finish a review, close/reopen the PR later, retain reviewed status for unchanged effective file diffs, identify files requiring re-review, collaborate with an agent through the documented `aside` skill, and explicitly publish selected feedback to GitHub only after seeing exactly what will be sent.

The **first implementation deliverable** is narrower: a polished, interactive, mocked frontend demonstrating all core scope-selection and reviewed-state interactions using contract-valid fixtures, before completing GitHub integration and publication.

---

## 14. Reference material

- Existing reference implementation: https://github.com/codesoda/discuss-cli
- Existing `discuss-cli` skill: https://github.com/codesoda/discuss-cli/blob/main/skills/discuss/SKILL.md
- GitHub CLI documentation: https://cli.github.com/manual/
- Axum: https://docs.rs/axum/
- Svelte: https://svelte.dev/
- Tailwind CSS: https://tailwindcss.com/
- shadcn-svelte: https://www.shadcn-svelte.com/

**Implementation note:** This PRD defines product and architecture requirements; it does not implement Aside. API names and example models are proposed contracts, not claims about existing Aside code.
