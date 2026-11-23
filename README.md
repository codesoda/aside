<a id="readme-top"></a>

<div align="center">
  <h1>Aside</h1>
  <p>A local-first workspace for reviewing GitHub pull requests with a human and a coding agent.</p>
  <p>
    <a href="docs/plans/aside-prd.md"><strong>Product requirements »</strong></a>
    · <a href="https://github.com/codesoda/aside/issues">Report a bug</a>
    · <a href="https://github.com/codesoda/aside/issues">Request a feature</a>
  </p>
</div>

<details>
<summary>Contents</summary>

- [About](#about)
- [Getting started](#getting-started)
- [Usage](#usage)
- [Development](#development)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [License](#license)
- [Acknowledgments](#acknowledgments)

</details>

## About

Aside opens a PR review in a browser served by a local Rust CLI. It combines changed files, inline discussions, repository-wide include/exclude rules, and review progress. A local agent can inspect the same review and reply to feedback; publishing a review to GitHub requires a human confirmation. Review state lives on your machine, not in the target repository.

This project is under active development. The backend and API are still being integrated; do not rely on it for publishing a production review until those checks pass. An uncertain or partial publication stays blocked until a human reconciles it on GitHub; automatic retries could duplicate a review.

### Built with

- [Rust](https://www.rust-lang.org/) and [Axum](https://github.com/tokio-rs/axum) for the CLI and loopback HTTP API.
- [SQLite](https://www.sqlite.org/) for local review state.
- [Svelte](https://svelte.dev/), [Vite](https://vite.dev/), and [Tailwind CSS](https://tailwindcss.com/) for the embedded UI.
- [GitHub CLI](https://cli.github.com/) for access to live PRs.

<p align="right"><a href="#readme-top">Back to top</a></p>

## Getting started

### Quick install

For a human on macOS ARM or Linux x86-64 (shell):

```sh
curl -fsSL https://raw.githubusercontent.com/codesoda/aside/main/install.sh | sh
```

For a human on Windows x86-64 (PowerShell):

```powershell
irm https://raw.githubusercontent.com/codesoda/aside/main/install.ps1 | iex
```

**These commands execute remote code.** Use them only if you trust the source; for a safer path, download and inspect the [Unix script](https://raw.githubusercontent.com/codesoda/aside/main/install.sh) or [PowerShell script](https://raw.githubusercontent.com/codesoda/aside/main/install.ps1) before running it. Agents must ask for consent and preferably inspect the script; they must not auto-run either command. The Windows quick installer currently supports x86-64 only. For other architectures, [build from source](#install-from-source) rather than using a release installer.

The installer installs Aside and its agent skill. Inspect is not required for native review and is not installed by default. After installation, it offers an optional live example (`[y/N]`) with `claude`, `codex`, or `pi`; declining does not start it. A live PR requires [GitHub CLI (`gh`)](https://cli.github.com/) authentication (`gh auth login`). The bundled offline demo (`aside demo`) is separate and does not require `gh`.

The installers resolve the [latest stable Aside release](https://github.com/codesoda/aside/releases/latest) and verify the matching asset against published SHA-256 checksums. If no stable release or matching asset is available, use the source-build instructions below. A source install requires an explicit `sh install.sh --from-source` or `./install.ps1 -FromSource` from the checkout.

### Optional: install Inspect separately

If you choose to install Inspect, download and review the installer first. Then run `sh install.sh --with-inspect` on macOS ARM or Linux x86-64, or `.\install.ps1 -WithInspect` on Windows x86-64. These flags also work with the source-install flags.

The installer downloads the standalone Inspect v0.1.1 executable directly from [Inspect’s GitHub Releases](https://github.com/Ataraxy-Labs/inspect/releases/tag/v0.1.1) and checks its pinned SHA-256. Inspect has its own [FSL-1.1-ALv2 licence](https://github.com/Ataraxy-Labs/inspect/blob/v0.1.1/LICENSE.md). Aside does not bundle or run Inspect. Without the opt-in flag, existing Inspect installations are left alone. If the optional download or verification fails, Aside stays installed. See the [Inspect evaluation](docs/inspect-evaluation.md).

### Prerequisites for building from source

- A current Rust toolchain (edition 2024 support) and Cargo.
- Node.js 24 and pnpm 10 or later to build the frontend.

### Install from source

```sh
git clone https://github.com/codesoda/aside.git
cd aside
pnpm -C app install --frozen-lockfile
pnpm -C app build
cargo build --locked --release
```

Run `target/release/aside` from the checkout, or put the executable on your `PATH`. Building the frontend first embeds the real UI into the binary; otherwise the build script creates a placeholder page. The supported quick-install targets are macOS ARM, Windows x86-64, and Linux x86-64.

<p align="right"><a href="#readme-top">Back to top</a></p>

## Usage

For optional plain-English explanations of selected changes, see
[Explain this change: local Ollama / Claude setup and privacy boundaries](docs/explain-change.md).
Q&A is opt-in and cannot edit files or change review status.

```sh
aside https://github.com/OWNER/REPO/pull/NUMBER
aside demo --no-open
aside update --check   # check without using the GitHub API
aside update           # ask before installing a verified release
aside --help
```

Only the human reviewer can select drafts, preview publication, and explicitly confirm a GitHub review. New and existing local drafts start selected. Deselecting a thread changes its local draft comments together; a selected reply under a deselected root stays local. The preview lists exact outbound destinations and bodies plus drafts kept local and their reasons. Selection or PR changes require a new preview. Omitted drafts keep their selection for a later review. A partial or uncertain attempt is never retried, reconciled, or cleared automatically: check GitHub before proceeding. Demo can select and preview locally but cannot confirm publication.

`aside demo` replays a bundled, offline capture of [cli/cli#14921](https://github.com/cli/cli/pull/14921); refresh uses a synthetic revision and cannot publish. Aside binds to `127.0.0.1` on an OS-assigned port and opens the review directly unless you pass `--no-open` or disable `browser.auto_open`. Keep the process running while you review. With `--no-open`, use the private login URL printed to stderr to open the UI; do not share that URL. It writes machine-readable session events to stdout and diagnostics to stderr. The agent-facing workflow and security boundaries are documented in [`skills/aside/SKILL.md`](skills/aside/SKILL.md).

Repository analysis controls are available through `aside get --repo github.com/OWNER/REPO` and `aside set analysis.history.enabled false --repo github.com/OWNER/REPO`. Refresh an existing session to apply changes. See [analysis configuration](docs/analysis-configuration.md) for budgets, capabilities, and privacy boundaries.

For unattended updates, `aside update --yes` is an explicit install confirmation. The updater downloads release assets and checks their published SHA-256 before replacing the CLI. It never queries the rate-limited GitHub API for the version check. The application stores its review database and private session descriptors in the OS user data directory. `ASIDE_DATA_DIR` overrides that location. Keep session descriptors private: they contain an agent credential.

<p align="right"><a href="#readme-top">Back to top</a></p>

## Development

```sh
pnpm -C app check
pnpm -C app build
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The CLI's Svelte frontend lives in `app/`; the standalone website lives in `web/`. Run `pnpm web` from the repository root to preview the website at `http://127.0.0.1:4173/` (set `ASIDE_WEB_PORT` to override the port). For a hot-reloaded CLI frontend backed by the server, run `pnpm -C app dev:aside` from the repository root. It builds and launches the demo server, starts Vite, and prints a private frontend login URL. Pass a full GitHub PR URL after `dev:aside` to test a live review. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for prerequisites, manual startup, port overrides, and shutdown instructions. The Rust backend is grouped by responsibility: `src/model/` defines the API contract, `src/diff/` parses patches, `src/github/` talks to `gh`, `src/store/` persists state, and `src/server/` serves authenticated routes. PRs run a Linux build and an [aislop](https://github.com/scanaislop/aislop) quality gate. Tags matching `v*` trigger the cross-platform release workflow. See [`aside-prd.md`](docs/plans/aside-prd.md) for contracts, security requirements, and acceptance criteria.

<p align="right"><a href="#readme-top">Back to top</a></p>

## Roadmap

- [x] CLI shell, offline UI shell, product requirements, and CI configuration.
- [ ] Complete and validate durable review state, authenticated API, and agent replies.
- [ ] Validate confirmed GitHub publishing and release artifacts on all target runners.

Track work and known issues in [GitHub Issues](https://github.com/codesoda/aside/issues).

<p align="right"><a href="#readme-top">Back to top</a></p>

## Contributing

Open an issue to discuss a change, then submit a pull request. Sign off each commit with `git commit -s` and follow the setup and checks in [`CONTRIBUTING.md`](CONTRIBUTING.md). Do not include private PR content or session credentials in fixtures or logs.

## License

No license file is included yet. Do not assume the repository grants open-source reuse rights until a license is added.

## Acknowledgments

README structure adapted from [Best-README-Template](https://github.com/othneildrew/Best-README-Template) by othneildrew. Aside uses the GitHub CLI for live reviews.

<p align="right"><a href="#readme-top">Back to top</a></p>
