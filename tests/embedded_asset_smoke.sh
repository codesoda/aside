#!/usr/bin/env bash
# Serve a release binary's embedded UI without Node on PATH.
# Does not print session descriptors or login URLs.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: tests/embedded_asset_smoke.sh /absolute/or/relative/path/to/aside" >&2
  exit 2
fi

bin=$1
if [[ ! -f "$bin" ]]; then
  echo "binary not found: $bin" >&2
  exit 1
fi

root=$(mktemp -d "${TMPDIR:-/tmp}/aside-smoke.XXXXXX")
pid=""
redact() {
  sed -E 's#(token=)[^[:space:]&"]+#\1[redacted]#g'
}
cleanup() {
  if [[ -n "$pid" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$root"
}
trap cleanup EXIT

export ASIDE_DATA_DIR="$root/data"
export ASIDE_CONFIG_DIR="$root/config"
mkdir -p "$ASIDE_DATA_DIR" "$ASIDE_CONFIG_DIR"

if [[ -n "${PATH:-}" ]]; then
  filtered=$(printf '%s\n' "$PATH" | tr ':' '\n' | grep -Ev '/node(/|$)|nodejs|/\.nvm/|fnm' || true)
  PATH=$(printf '%s\n' "$filtered" | paste -sd: -)
  export PATH
fi
command -v curl >/dev/null 2>&1 || {
  echo "curl is required for the embedded UI smoke" >&2
  exit 1
}

"$bin" demo --no-open >"$root/out" 2>"$root/err" &
pid=$!

url=""
for _ in $(seq 1 100); do
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "aside exited before serving the embedded UI" >&2
    redact <"$root/err" >&2 || true
    exit 1
  fi
  url=$(sed -n 's/.*"event":"session.started".*"url":"\([^"]*\)".*/\1/p' "$root/out" | head -n 1 || true)
  if [[ -n "$url" ]]; then
    break
  fi
  sleep 0.1
done

if [[ -z "$url" ]]; then
  echo "aside did not emit session.started" >&2
  redact <"$root/err" >&2 || true
  exit 1
fi

body=$(curl -fsS --max-time 5 "$url")
printf '%s' "$body" | grep -q 'id="app"' || {
  echo "embedded UI is missing the review app mount point" >&2
  exit 1
}
if printf '%s' "$body" | grep -q 'Frontend not built'; then
  echo "binary embedded the placeholder UI; run pnpm -C app build before cargo build --release" >&2
  exit 1
fi

# session.started url has no credential. Build the dot-path from the origin
# and path only, so a future query string cannot be mistaken for the path.
origin=${url%%\#*}
origin=${origin%%\?*}
origin=${origin%/}
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "${origin}/.git/config")
if [[ "$code" != "404" ]]; then
  echo "dot-path request returned $code, expected 404" >&2
  exit 1
fi
