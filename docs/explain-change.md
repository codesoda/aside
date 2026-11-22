# Explain this change

On-demand, plain-English Q&A for a selected diff. This is separate from background
analysis and review comments. It cannot mark files reviewed, publish reviews, edit
files, or give the model tools. No requests are sent when opening a review.

## Setup

Build/install an Aside binary containing this feature first (see the README's
source-build instructions). Older installed binaries do not recognize the new
`ai.*` settings; the commands below refer to the newly built binary on your PATH.

Use Aside's existing **user** TOML configuration / `aside set` interface, not a
file in the repository being reviewed. `ASIDE_CONFIG_DIR` can override the OS
configuration directory. Restart Aside after changing settings.

### Local models: Ollama

Run a trusted Ollama server yourself, with cloud features disabled
(`OLLAMA_NO_CLOUD=1`), and install a local text model appropriate for your machine.
Aside does **not** start the daemon, pull models, or download software.

```sh
aside set ai.local_url http://127.0.0.1:11434
aside set ai.local_model YOUR_INSTALLED_LOCAL_MODEL
```

Only HTTP loopback **IP addresses** are accepted (`127.0.0.1`, `[::1]`), not
hostnames, LAN hosts, URL credentials or custom paths. Requests bypass HTTP proxies
and never follow redirects. Before sending code, Aside calls `/api/show` and
requires local model architecture metadata with no `remote_model`/`remote_host`.
Remote/cloud models and unknown metadata are rejected; there is no hosted fallback.
The subsequent `/api/chat` request is non-streaming and supplies no tools.

Trust boundary: the configured local service must really be Ollama running local
weights. Aside cannot prevent a malicious local service from forwarding data, or
an operator from replacing the model between inspection and generation. Disable
Ollama cloud features and do not expose or change the daemon during a request.
This initial adapter is **Ollama-specific**, not a generic OpenAI-compatible endpoint.
A configured option means setup is present, not that the daemon/model is reachable;
reachability is checked only when sending a question.

### Claude Code (hosted)

Install and authenticate Claude Code yourself using its supported login flow, then:

```sh
aside set ai.claude_enabled true
aside set ai.claude_model sonnet
```

The CLI must be on Aside's `PATH` and support the options discovered in Claude Code
**2.1.330**: `--print`, `--output-format json`, `--tools ""`, `--safe-mode`,
`--strict-mcp-config`, `--mcp-config`, `--disable-slash-commands`,
`--no-session-persistence`, `--setting-sources`, `--settings`, `--no-chrome`, and
`--permission-mode dontAsk`. Capability checks use `claude --help`, not inference.
Older unsupported CLIs are disabled; Aside never silently drops safety flags.

Each request uses an isolated temporary directory, no tools, no MCP servers, no
skills, no project/user customizations and disabled hooks. Existing CLI
credentials remain in the CLI's own trusted authentication store; Aside neither
reads nor copies them. We deliberately do not use `--bare`: that mode excludes
OAuth/keychain auth and would not reuse a subscription login. CLI installations
and administrator-managed policies remain trusted; do not use an untrusted CLI
wrapper. Custom user hooks/plugins/settings are intentionally not reused.

The selection and conversation go to the CLI's configured hosted service. Quota,
retention and charging policies of that service apply. No hosted API key is
requested or stored by Aside, and no direct Anthropic API adapter is provided.

### Codex

Codex is displayed as **unavailable**, not a mock integration. Discovery against
`codex-cli 0.149.0`, `codex exec --help` and its versioned configuration/app-server
documentation did not establish a supported blanket no-tools interface. A
`read-only` sandbox still permits command execution and therefore does not satisfy
this feature's boundary. A working adapter needs a verified no-tools invocation
or a separately approved inference-only API/authentication arrangement. No Codex
process or model request is launched by the feature.

## Usage

1. Open **Files changed**. Expand **How to read this diff** for the colour/symbol guide.
2. Use **Explain** beside an added/removed line, or **Explain this change** on a
   change-block header. A line includes its adjacent removed/added counterpart.
3. Choose a configured provider. Check the **local / hosted** label before sending.
4. Press **Explain this change**, or type a question and press **Send question**.
   Selecting a provider or suggested question alone does not send anything.
5. Ask follow-ups in the same panel. Switching provider clears the conversation;
   switching file/selection closes or replaces it. Closing returns focus to the
   trigger without scrolling the review. Escape closes the non-modal panel.

The panel displays plain text, not executable HTML/Markdown. Red/minus is removed
code and green/plus is added code, **not** error or safety evidence. Model answers
must distinguish facts, possible effects, checks and uncertainty. They may still
be incorrect; always inspect the cited lines and relevant tests yourself.

## Context and lifecycle

The browser sends identity only: session, base/head SHA, file ID, diff digest,
hunk ID and optional line ID. The server resolves the selection from its snapshot.
Client-supplied code, filesystem paths, arbitrary endpoints and commands are not
accepted. Binary/incomplete patches and oversized changes fail explicitly.

Context includes repository/PR identity, old/new line numbers and text, at most
three surrounding patch lines, and up to ten overlapping current-head check
annotations. It excludes whole files, other files, PR descriptions and review
comments. Diagnostics may be missing/partial and do not constitute a full check.
Repository content, annotations and prior answers are untrusted data, separated
from the system instructions. No model-callable tools are provided.

Limits: 160 patch lines / 24 KB context; 2 KB question; eight prior Q&A pairs;
64 KB total prompt; 16 KB answer; 128 KB provider output. Larger inputs fail rather
than silently dropping the selected code or previous turns. The conversation is
browser-memory-only, not a durable review artifact. A provider may retain requests
under its own policy even though Aside does not persist them.

At most one generation runs per review. The server deadline is 90 seconds. Cancel
sends an explicit cancellation request as well as aborting browser HTTP; dropping
the provider operation closes HTTP IO or kills the CLI child. Cancellation before
request arrival is remembered for 120 seconds. Provider-side processing/charges
may already have happened. If cancellation cannot be confirmed, the UI says so
and the server timeout remains in force. The review stays usable throughout.

The server checks selection identity both before and after inference. A changed
session, base/head revision, file digest or selection rejects the answer. The UI
also clears stale answers and ignores late/cancelled/mismatched replies.

## API (authenticated human reviewer only)

- `GET /api/v1/explain/providers`: supported/configured options, model, processing
  location and setup guidance. No credentials or endpoint secrets returned.
- `POST /api/v1/explain/questions`: `{ requestId, provider, selection, question,
  history: [{ question, answer }] }`. Returns `{ requestId, provider, selection,
  answer }`. Uses existing UI-cookie and same-origin protection. Agent tokens
  cannot trigger paid inference or cancellation.
- `POST /api/v1/explain/questions/{requestId}/cancel`: cancels the operation.

Errors use the existing `{ error, message, details }` envelope: stale selection
409, invalid setup/input 400, busy 429, provider failure 502, timeout 504.
No provider stderr or raw error payload is exposed to the browser.

## Validation

```sh
pnpm -C app check
pnpm -C app test
pnpm -C app build
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

`app/src/explain.test.mjs` mounts the actual review UI with synthetic HTTP
responses. Rust context/provider/service/API tests use synthetic snapshots,
loopback fake Ollama responses, and (Unix) harmless subprocess fixtures. These
are **not live model-quality or authentication tests**.

Before release, run an explicitly approved smoke test with a configured local
model, then with the supported authenticated Claude CLI. Use synthetic code:
`- let max = 5; / + let max = 10;`. Check that an initial explanation distinguishes
removal/addition from errors, follow-ups reference the same lines, cancellation
leaves the review usable, and no unrelated files/review state are accessed.
Also inspect the panel at mobile/desktop sizes and with keyboard navigation.
Do not launch an Aside demo or make a hosted request without the operator's consent.

### Explicit live Claude smoke test

Only after the operator approves two hosted requests and any associated usage:

```sh
ASIDE_LIVE_CLAUDE_SMOKE=1 cargo test --locked \
  server::explain_live_tests::claude_synthetic_smoke -- --ignored --exact --nocapture
```

This ignored test uses the actual authenticated API handler and Claude adapter,
with an in-memory opt-in configuration and a completely synthetic repository,
review and diff (`retry_limit()` returns 5 before, 10 after). It asks for an initial
explanation and one follow-up, checks selection identity and unchanged review
state, and prints the answers for human inspection. It uses the installed CLI's
existing authentication without reading credentials itself. It does not load or
modify global Aside configuration, launch an Aside demo, use real review content,
or make a hosted request during ordinary `cargo test` runs. The environment gate
also prevents accidental calls through `--include-ignored`. It is an API/adapter
smoke test, not a live browser test or a general model-quality evaluation.

A live run using the Sonnet alias completed both requests after CLI sign-in:
the answers identified 5 → 10, explained that red means removal rather than an
error, retained the selected change on follow-up, and left review state unchanged.
Manual inspection also caught an overclaim that diagnostics did not *exist*,
rather than that none were supplied. The system prompt was subsequently tightened
to require the latter wording and avoid Markdown. A subsequent two-request live
run tested the final prompt: both answers used plain text and correctly said no
matching diagnostics were supplied, without inferring that no problems exist.
The follow-up retained the selected values and uncertainty about callers; review
state remained unchanged. These runs verify the integration and these synthetic
examples, not unconditional answer accuracy.

### Real-browser UI validation

Chromium validation of the built UI with synthetic API responses passed 33 checks
at desktop (1440 × 1000) and mobile (390 × 844) sizes, with light/dark screenshots.
This covered explicit-send behavior, follow-up context, cancellation/retry,
provider errors, inert HTML, stale/mismatched replies, keyboard access, focus
restoration, scroll preservation on opening, and unchanged review state. Manual
mobile inspection prompted a sticky panel heading so Close remains visible while
scrolling. No external browser requests or unexpected console/page errors occurred.
These UI checks used mocked provider responses, not live inference. Live local
Ollama validation still requires an installed model and is not claimed here.

## Interface references

- Installed `claude --help` (2.1.330), [Claude CLI reference](https://code.claude.com/docs/en/cli-reference)
  and [Claude settings](https://code.claude.com/docs/en/settings).
- [Ollama 0.40.16 API](https://github.com/ollama/ollama/blob/v0.40.16/docs/api.md)
  and [response types](https://github.com/ollama/ollama/blob/v0.40.16/api/types.go).
- Installed `codex exec --help` (0.149.0), [versioned schema](https://github.com/openai/codex/blob/rust-v0.149.0/codex-rs/core/config.schema.json)
  and [app-server interface](https://github.com/openai/codex/blob/rust-v0.149.0/codex-rs/app-server/README.md).

Pi is the development harness for this change, not an additional runtime provider.
