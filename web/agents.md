# Set up Aside with a coding agent

Aside is a local-first workspace for reviewing pull requests. This guide is for the agent helping a person install it. The homepage prompt points to `https://asidecode.dev/agents.md`. Deploy this guide at `/agents.md` on that domain before using the prompt publicly.

## Check the machine and ask first

1. Explain what Aside does and ask which machine/OS and architecture the person uses. Supported release targets are **macOS ARM**, **Windows x86-64 and ARM64**, and **Linux x86-64**. Do not suggest a release installer for another target; refer to [building from source](https://github.com/codesoda/aside#install-from-source) instead.
2. Ask for the person's consent before downloading or running an installer. Download the official script to a local file, **inspect it with the person**, and run it only after they approve what it does. Do not pipe network code into a shell or run an uninspected script. If the scripts or matching release artifacts are not yet published, stop and explain the blocker; do not substitute an unofficial installer.
3. For a live pull request, check that [GitHub CLI (`gh`)](https://cli.github.com/) is installed and authenticated (`gh auth status`). If needed, guide the person through `gh auth login` themselves. Do not ask them to share credentials. The bundled demo runs offline without `gh` or GitHub authentication.

## Install after inspection and consent

The official scripts are hosted in the [Aside repository](https://github.com/codesoda/aside):

- macOS ARM or Linux x86-64 (shell): `https://raw.githubusercontent.com/codesoda/aside/main/install.sh`

  ```sh
  curl -fL https://raw.githubusercontent.com/codesoda/aside/main/install.sh -o aside-install.sh
  less aside-install.sh
  # Only after the person approves the inspected script:
  sh aside-install.sh
  ```

- Windows x86-64 or ARM64 (PowerShell): `https://raw.githubusercontent.com/codesoda/aside/main/install.ps1`

  ```powershell
  Invoke-WebRequest https://raw.githubusercontent.com/codesoda/aside/main/install.ps1 -OutFile aside-install.ps1
  Get-Content .\aside-install.ps1
  # Only after the person approves the inspected script:
  .\aside-install.ps1
  ```

Review any redirects, downloads, destination paths, permission changes, and release integrity checks in the script before approving. If the install requires further approval or environment changes, explain them first. Do not bypass local security policies.

The installer installs the `aside` binary and the built-in **`aside skill`** for agent workflows automatically. Do not fetch an unrelated skill or copy secret session data into the repository. Confirm `aside --help` works after installation; if the skill is not available, report that instead of claiming setup is complete.

## Start a review

For a live PR, after `gh` is ready, use `aside https://github.com/OWNER/REPO/pull/NUMBER`. Ask the person **before** running `aside demo` (or `aside demo --no-open`); it opens a bundled offline example and does not publish. Do not start the demo just to verify installation without permission. The website's interactive mock is a separate preview.

Keep the local session private. A browser login URL and the agent session descriptor/token are credentials: never print, paste, commit, or transmit them to another service. Follow the installed `aside skill` for session and API steps, and let the person make all publication decisions. Aside requires human confirmation to publish a review.
