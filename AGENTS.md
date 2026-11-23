# Agent instructions

## Isolated development data

- During scoping, planning, implementation, testing, debugging, and PR review, run Aside with an explicit, isolated `ASIDE_DATA_DIR`. Never use the default shared application database for development work.
- Create a private directory outside the repository for each worktree/task. For example: `export ASIDE_DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/aside-dev.XXXXXX")"`. Pass the same value to every related CLI, server, dev script, and subagent process; do not assume separate shells inherit it.
- Never share this directory between independent worktrees or PRs. Their database migrations may be incompatible. Reuse it only for the same task when testing persistence or restart behavior.
- Keep database files, session descriptors, tokens, and private logs out of Git. Isolated state does not include the user's saved reviews; state that limitation when it matters.
- Test migrations against disposable fixtures or copies inside the isolated directory. Never delete, downgrade, or manually migrate the shared user database to resolve a development schema mismatch.
- Preserve isolated data needed for an active review or investigation. Remove it only when the task no longer needs it and all processes using it have stopped.

## Issue workflow

- Before assigning an issue to yourself, double-check whether it is already assigned. Do not take an issue assigned to someone else without coordinating with them.
- If the issue is a subissue, check whether its parent issue has a branch. Use that branch as the stacking base when appropriate.
- When you start work on an issue, set its status to **In Progress**.
- Add a technical plan to the issue before substantial implementation. Keep it updated if the approach changes.
- Post regular progress comments on the issue as you work, including meaningful milestones and blockers.
- When you create a PR, include `Fixes #ISSUE` in its description, replacing `ISSUE` with the actual issue number. If no issue number is known, ask for one before creating the PR; never leave the placeholder in the description.
- Stacked PRs are allowed for dependent issues. Make the dependency and base branch clear in each PR.
