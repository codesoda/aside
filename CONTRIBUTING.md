# Contributing to Aside

Thanks for helping improve Aside. Open an issue before a large change so we can agree on its scope. Keep PRs focused and add tests for changed behavior.

## Sign off each commit

Add a `Signed-off-by: Your Name <your-email@example.com>` trailer to every commit. Git can add it from your configured name and email:

```sh
git config user.name "Your Name"
git config user.email "your-email@example.com"
git commit -s -m "Describe the change"
```

Check the trailer with `git log -1 --format=%B` before pushing. This is a sign-off, **not** a requirement for SSH or GPG cryptographic signing. Do not commit credentials, session descriptors, or private PR content.

## Prerequisites

- Rust with edition 2024 support, Node.js 24, and pnpm 10 or later.
- For live PRs: install `gh` and run `gh auth login`. The demo works offline.
- For workflow validation: install `actionlint`, or leave that check to CI.

```sh
pnpm -C app install --frozen-lockfile
```

## Start the backend and frontend together

From the repository root:

```sh
pnpm -C app dev:aside
```

This builds the debug CLI, launches `aside demo --no-open`, starts Vite, and prints a **private frontend login URL**. Open that URL to review the bundled capture of [cli/cli#14921](https://github.com/cli/cli/pull/14921). Press Ctrl-C to stop both processes. The backend uses an OS-assigned port; the script discovers it from the `session.started` event. Do not guess the port or share the login URL.

To review a live PR, provide its full URL:

```sh
pnpm -C app dev:aside https://github.com/OWNER/REPO/pull/NUMBER
```

Vite uses port 5173 by default. If it is occupied, set `ASIDE_DEV_PORT=5174` (or another free port) before running the command. For an agent that parses JSON stdout, run `node scripts/dev.mjs` directly: the script forwards backend JSON events and emits `frontend.ready` with the Vite URL on stdout. Vite diagnostics go to stderr. The `pnpm` wrapper may print its own banner.

## Start each process manually

In terminal 1, start the server:

```sh
ASIDE_DEV_ORIGIN=http://127.0.0.1:5173 cargo run --locked -- demo --no-open
```

Copy the exact backend URL from the `session.started` JSON event. In terminal 2, start Vite with that URL (including its trailing slash):

```sh
ASIDE_URL=http://127.0.0.1:ACTUAL_BACKEND_PORT/ pnpm -C app dev
```

Open the **Vite-origin** `/auth?token=…` URL using the path and query from the server's private login URL on stderr. The backend URL itself serves the embedded production UI, not Vite's hot-reloaded frontend. If you choose a different Vite port, set the matching `ASIDE_DEV_PORT` and `ASIDE_DEV_ORIGIN` in these commands.

## Preview the standalone website

The marketing website in `web/` is separate from the CLI frontend in `app/`. From the repository root, run `pnpm web` and open `http://127.0.0.1:4173/`. Set `ASIDE_WEB_PORT` to choose another port. Press Ctrl-C to stop it. The website preview needs no dependency installation.

## Checks before a PR

```sh
pnpm -C app check
pnpm -C app build
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
actionlint .github/workflows/*.yml
```

Do not test GitHub publication against another person's PR. Use the offline demo or fake GitHub runner for mutation tests. See [`aside-prd.md`](aside-prd.md) for the behavior and security contracts.
