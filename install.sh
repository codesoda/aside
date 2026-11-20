#!/bin/sh
set -eu

REPO=https://github.com/codesoda/aside
INSTALL_DIR="${HOME}/.aside/bin"
LINK_DIR="${HOME}/.local/bin"
BINARY="${INSTALL_DIR}/aside"
LINK="${LINK_DIR}/aside"
TMP_DIR=

cleanup() { [ -z "$TMP_DIR" ] || rm -rf "$TMP_DIR"; }
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }
require() { command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"; }

target() {
  case "$(uname -s):$(uname -m)" in
    Darwin:arm64|Darwin:aarch64) printf '%s' aarch64-apple-darwin ;;
    Linux:x86_64|Linux:amd64) printf '%s' x86_64-unknown-linux-gnu ;;
    *) fail 'unsupported platform; releases support Apple Silicon macOS and x86_64 Linux (use install.ps1 on Windows)' ;;
  esac
}

latest_tag() {
  # Resolve the public redirect, never the rate-limited GitHub API.
  url=$(curl -fsSIL --max-time 30 -o /dev/null -w '%{url_effective}' "$REPO/releases/latest") || fail 'cannot resolve latest release'
  case "$url" in
    "$REPO/releases/tag/"v*) tag=${url##*/} ;;
    *) fail "unexpected latest release redirect: $url" ;;
  esac
  case "$tag" in *[!a-zA-Z0-9._-]*|v|v[!0-9]*) fail "invalid release tag: $tag" ;; esac
  printf '%s' "$tag"
}

download() {
  code=$(curl -sSL --max-time 120 --retry 2 -o "$2" -w '%{http_code}' "$1") || fail "download failed: $1"
  case "$code" in 2??) : ;; *) fail "download returned HTTP $code: $1" ;; esac
}

checksum() {
  awk -v name="$2" '
    $2 == name && $1 ~ /^[0-9a-fA-F]+$/ && length($1) == 64 && NF == 2 { hash=$1; count++ }
    $2 == name && !($1 ~ /^[0-9a-fA-F]+$/ && length($1) == 64 && NF == 2) { bad=1 }
    END { if (count != 1 || bad) exit 1; print hash }
  ' "$1" || fail "expected exactly one valid checksum for $2"
}

install_binary() {
  [ ! -L "$BINARY" ] || fail "refusing to replace symlink $BINARY"
  [ ! -e "$BINARY" ] || [ -f "$BINARY" ] || fail "refusing to replace $BINARY"
  if [ -e "$LINK" ] || [ -L "$LINK" ]; then
    [ -L "$LINK" ] && [ "$(readlink "$LINK")" = "$BINARY" ] || fail "refusing to replace unrelated $LINK"
  fi
  for dir in "$HOME/.aside" "$INSTALL_DIR" "$HOME/.local" "$LINK_DIR"; do
    [ ! -L "$dir" ] || fail "refusing to install through symlink $dir"
  done
  mkdir -p "$INSTALL_DIR" "$LINK_DIR"
  install -m 0755 "$1" "$BINARY"
  if [ "$(uname -s)" = Darwin ]; then
    if command -v xattr >/dev/null 2>&1; then
      xattr -d com.apple.quarantine "$BINARY" 2>/dev/null || :
    else
      printf 'warning: xattr unavailable; macOS may quarantine %s\n' "$BINARY" >&2
    fi
  fi
  ln -sfn "$BINARY" "$LINK"
}

require uname
require install
require mktemp
TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/aside-install.XXXXXXXX") || fail 'cannot create temporary directory'

# A downloaded script has no local Cargo.toml; a checkout can build without a release.
if [ -f "$0" ] && [ -f "$(dirname "$0")/Cargo.toml" ] && [ -f "$(dirname "$0")/skills/aside/SKILL.md" ]; then
  require cargo
  require pnpm
  root=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
  pnpm -C "$root/app" install --frozen-lockfile || fail 'frontend dependency install failed'
  pnpm -C "$root/app" build || fail 'frontend build failed'
  (cd "$root" && cargo build --locked --release) || fail 'source build failed'
  [ -f "$root/target/release/aside" ] || fail 'source build did not produce aside'
  install_binary "$root/target/release/aside"
else
  require curl
  require tar
  require awk
  if command -v sha256sum >/dev/null 2>&1; then
    hash_file() { sha256sum "$1" | awk '{print $1}'; }
  else
    require shasum
    hash_file() { shasum -a 256 "$1" | awk '{print $1}'; }
  fi
  platform=$(target)
  tag=$(latest_tag)
  asset="aside-$tag-$platform.tar.gz"
  base="$REPO/releases/download/$tag"
  download "$base/checksums-sha256.txt" "$TMP_DIR/checksums-sha256.txt"
  download "$base/$asset" "$TMP_DIR/$asset"
  expected=$(checksum "$TMP_DIR/checksums-sha256.txt" "$asset")
  actual=$(hash_file "$TMP_DIR/$asset") || fail 'cannot hash archive'
  [ "$actual" = "$(printf '%s' "$expected" | tr 'A-F' 'a-f')" ] || fail "SHA256 mismatch for $asset; refusing to install"
  # Never extract archive paths to disk. Require one regular file with the exact name.
  entries=$(tar -tzf "$TMP_DIR/$asset") || fail 'invalid archive'
  [ "$entries" = aside ] || fail 'archive must contain only aside'
  kind=$(tar -tvzf "$TMP_DIR/$asset" | awk 'NR == 1 { print substr($0,1,1) }') || fail 'invalid archive'
  [ "$kind" = - ] || fail 'archive entry is not a regular file'
  tar -xOzf "$TMP_DIR/$asset" aside > "$TMP_DIR/aside" || fail 'cannot read archive binary'
  [ -s "$TMP_DIR/aside" ] || fail 'archive binary is empty'
  install_binary "$TMP_DIR/aside"
fi

"$BINARY" --version || fail 'installed binary did not run'
"$BINARY" skill || fail 'embedded skill installation failed'
printf 'Installed aside at %s (linked at %s).\n' "$BINARY" "$LINK"
case ":${PATH:-}:" in *":$LINK_DIR:"*) : ;; *) printf 'Add %s to PATH to use aside from a new terminal.\n' "$LINK_DIR" ;; esac
if [ -t 0 ]; then
  printf 'Run the interactive aside demo now? [y/N] '
  IFS= read -r answer || answer=
  case "$answer" in y|Y|yes|YES|Yes) "$BINARY" demo ;; *) printf 'Run aside demo when ready.\n' ;; esac
else
  printf 'Host agent: ask the user whether to run aside demo; run it only with explicit approval.\n'
fi
