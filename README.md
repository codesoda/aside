<a id="readme-top"></a>

<div align="center">
  <h1>Aside</h1>
  <p>A local-first workspace for reviewing GitHub pull requests with a human and a coding agent.</p>
  <p>
    <a href="aside-prd.md"><strong>Product requirements »</strong></a>
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

### Install with an agent

Give your coding agent the setup prompt on the [Aside website](web/index.html), then follow the [agent setup guide](web/agents.md). The prompt's `https://asidedomain/agents.md` is a placeholder; the website deployer must replace `asidedomain` with the actual production domain and serve the guide at `/agents.md`. Supported release targets are Mac ARM, Windows x86-64/ARM64, and Linux x86-64. The guide uses official install scripts only after download, inspection, and your consent. Ask before running the offline demo. The installer also installs the built-in `aside skill` for agents.

For live PRs, install and authenticate [GitHub CLI (`gh`)](https://cli.github.com/) with `gh auth login`. The offline demo does not need `gh` or GitHub authentication. If the official scripts or release artifacts have not been published, build from source instead.

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

Run `target/release/aside` from the checkout, or put the executable on your `PATH`. Building the frontend first embeds the real UI into the binary; otherwise the build script creates a placeholder page. Tagged releases are intended to include prebuilt archives for Mac ARM, Windows x86/ARM, and Linux when the release workflow passes.

<p align="right"><a href="#readme-top">Back to top</a></p>

## Usage

```sh
aside https://github.com/OWNER/REPO/pull/NUMBER
aside demo --no-open
aside update --check   # check without using the GitHub API
aside update           # ask before installing a verified release
aside --help
```

`aside demo` replays a bundled, offline capture of [cli/cli#14921](https://github.com/cli/cli/pull/14921); refresh uses a synthetic revision and cannot publish. Aside binds to `127.0.0.1` on an OS-assigned port and opens the review directly unless you pass `--no-open` or disable `browser.auto_open`. Keep the process running while you review. With `--no-open`, use the private login URL printed to stderr to open the UI; do not share that URL. It writes machine-readable session events to stdout and diagnostics to stderr. The agent-facing workflow and security boundaries are documented in [`skills/aside/SKILL.md`](skills/aside/SKILL.md).

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

The CLI's Svelte frontend lives in `app/`; the standalone website lives in `web/`. Run `pnpm web` from the repository root to preview the website at `http://127.0.0.1:4173/` (set `ASIDE_WEB_PORT` to override the port). For a hot-reloaded CLI frontend backed by the server, run `pnpm -C app dev:aside` from the repository root. It builds and launches the demo server, starts Vite, and prints a private frontend login URL. Pass a full GitHub PR URL after `dev:aside` to test a live review. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for prerequisites, manual startup, port overrides, and shutdown instructions. The Rust backend is grouped by responsibility: `src/model/` defines the API contract, `src/diff/` parses patches, `src/github/` talks to `gh`, `src/store/` persists state, and `src/server/` serves authenticated routes. PRs run a Linux build and an [aislop](https://github.com/scanaislop/aislop) quality gate. Tags matching `v*` trigger the cross-platform release workflow. See [`aside-prd.md`](aside-prd.md) for contracts, security requirements, and acceptance criteria.

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

README structure adapted from [Best-README-Template](https://github.com/othneildrew/Best-README-Template) by othneildrew. Aside uses the GitHub CLI and the open-source projects listed above.

<p align="right"><a href="#readme-top">Back to top</a></p>
