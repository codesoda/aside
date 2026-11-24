# Set up Aside with a coding agent

Aside is a local-first workspace for reviewing pull requests. This guide is for agents helping a person install it; it is not permission to execute a remote script.

## Check the machine and ask first

1. Confirm the OS and architecture. Supported quick-install targets are **macOS ARM**, **Linux x86-64**, and **Windows x86-64**. For any other target, do not run a release installer; offer [building from source](https://github.com/codesoda/aside#install-from-source).
2. Explain that the human quick-install commands below execute code from the network. Ask for explicit consent before downloading or running any script. Prefer downloading the official script to a local file, inspecting it with the person, and running it only after approval. **Never automatically run a remote script or pipe it into a shell as an agent.** Check redirects, downloads, destinations, permission changes, and integrity checks. Do not bypass local security policies.
3. For a live PR, check [GitHub CLI (`gh`)](https://cli.github.com/) and `gh auth status`. Guide the person through `gh auth login` if needed; never request credentials. The separate bundled offline demo does not require `gh`.

## Install after consent

The official [Unix script](https://raw.githubusercontent.com/codesoda/aside/main/install.sh) and [PowerShell script](https://raw.githubusercontent.com/codesoda/aside/main/install.ps1) resolve the latest stable Aside release and verify its SHA-256 checksum. They install Aside and its agent skill. Review the script and its downloads with the person before asking for consent. If no release or matching asset is available, stop and offer source-build instructions rather than another installer.

For a human who trusts the source, the quick commands are:

```sh
curl -fsSL https://raw.githubusercontent.com/codesoda/aside/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/codesoda/aside/main/install.ps1 | iex
```

These are **human choices**, not agent instructions. Prefer these inspect-first alternatives when assisting:

```sh
curl -fsSL https://raw.githubusercontent.com/codesoda/aside/main/install.sh -o aside-install.sh
less aside-install.sh
# Only with the person's approval after inspection:
sh aside-install.sh
```

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/codesoda/aside/main/install.ps1 -OutFile aside-install.ps1
Get-Content .\aside-install.ps1
# Only with the person's approval after inspection:
.\aside-install.ps1
```

A checkout source install is an explicit alternative (`sh install.sh --from-source` or `./install.ps1 -FromSource`), never the default. Verify `aside --help` and that the installed skill is available; report a failure rather than claiming setup is complete.

## Start a review

The installer offers an optional live example after a `[y/N]` prompt using `claude`, `codex`, or `pi`. Do not answer yes or start an example without the person's approval. For a live PR, use `aside https://github.com/OWNER/REPO/pull/NUMBER` after `gh` is ready. The separate `aside demo` is bundled and offline; ask before running it too. The website's interactive mock is only a preview.

Keep local login URLs and session descriptors/tokens private. Never print, paste, commit, or send them to another service. Follow the installed Aside skill for session steps and leave publication decisions to the person. Publishing a review requires human confirmation.
