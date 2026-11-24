# Set up Aside with a coding agent

Aside is a local-first workspace for reviewing pull requests. This guide is for agents helping a person install it; it is not permission to execute a remote script.

## Check the machine and ask first

1. Confirm the OS and architecture. Supported quick-install targets are **macOS ARM**, **Linux x86-64**, and **Windows x86-64**. For any other target, do not run a release installer; offer [building from source](https://github.com/codesoda/aside#install-from-source).
2. Explain that the human quick-install commands below execute code from the network. Ask for explicit consent before downloading or running any script. Prefer downloading the official script to a local file, inspecting it with the person, and running it only after approval. **Never automatically run a remote script or pipe it into a shell as an agent.** Check redirects, downloads, destinations, permission changes, and integrity checks. Do not bypass local security policies.

## Install after consent

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
