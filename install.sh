#!/bin/sh
set -eu

REPO=https://github.com/codesoda/aside
INSTALL_DIR="${HOME}/.aside/bin"
LINK_DIR="${HOME}/.local/bin"
BINARY="${INSTALL_DIR}/aside"
LINK="${LINK_DIR}/aside"
INSPECT_BINARY="${INSTALL_DIR}/inspect"
INSPECT_LINK="${LINK_DIR}/inspect"
# Inspect v0.1.1 is licensed FSL-1.1-ALv2.
INSPECT_RELEASE=https://github.com/Ataraxy-Labs/inspect/releases/download/v0.1.1
INSPECT_LICENSE=https://github.com/Ataraxy-Labs/inspect/blob/v0.1.1/LICENSE.md
TMP_DIR=

transaction=0
aside_written=0
aside_link_touched=0
cleanup() {
  status=$?
  if [ "$transaction" = 1 ] && [ "$status" -ne 0 ]; then
    if [ "$aside_link_touched" = 1 ]; then
      rm -f "$LINK" || :
      [ ! -L "$TMP_DIR/aside.link.backup" ] || mv "$TMP_DIR/aside.link.backup" "$LINK" || :
    fi
    if [ "$aside_written" = 1 ]; then
      rm -f "$BINARY" || :
      [ ! -f "$TMP_DIR/aside.backup" ] || mv "$TMP_DIR/aside.backup" "$BINARY" || :
    fi
  fi
  [ -z "$TMP_DIR" ] || rm -rf "$TMP_DIR"
}
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

preflight_binary() {
  [ ! -L "$1" ] || fail "refusing to replace symlink $1"
  [ ! -e "$1" ] || [ -f "$1" ] || fail "refusing to replace $1"
  if [ -e "$2" ] || [ -L "$2" ]; then
    [ -L "$2" ] && [ "$(readlink "$2")" = "$1" ] || fail "refusing to replace unrelated $2"
  fi
}

preflight_install() {
  for dir in "$HOME/.aside" "$INSTALL_DIR" "$HOME/.local" "$LINK_DIR"; do
    [ ! -L "$dir" ] || fail "refusing to install through symlink $dir"
    [ ! -e "$dir" ] || [ -d "$dir" ] || fail "refusing to install through non-directory $dir"
  done
  preflight_binary "$BINARY" "$LINK"
}

install_verified() {
  preflight_install
  install -m 0755 "$1" "$TMP_DIR/aside.staged"
  [ ! -f "$BINARY" ] || cp -p "$BINARY" "$TMP_DIR/aside.backup"
  mkdir -p "$INSTALL_DIR" "$LINK_DIR"
  transaction=1
  aside_written=1
  mv "$TMP_DIR/aside.staged" "$BINARY"
  if [ "$(uname -s)" = Darwin ]; then
    if command -v xattr >/dev/null 2>&1; then
      xattr -d com.apple.quarantine "$BINARY" 2>/dev/null || :
    else
      printf 'warning: xattr unavailable; macOS may quarantine installed binaries\n' >&2
    fi
  fi
  [ ! -L "$LINK" ] || mv "$LINK" "$TMP_DIR/aside.link.backup"
  aside_link_touched=1
  ln -s "$BINARY" "$LINK"
}

install_inspect() (
  inspect_target() {
    case "$(uname -s):$(uname -m)" in
      Darwin:arm64|Darwin:aarch64)
        inspect_asset=inspect-macos-aarch64
        inspect_hash=e7fed5722af6e14dc668279dd7854109f9778484d48b1a42ead5d2c71b8bb90d ;;
      Linux:x86_64|Linux:amd64)
        inspect_asset=inspect-linux-x86_64
        inspect_hash=99cf4ea2a2a1048d8e9369a6a5a11e5f84ee3f3c706e0bde072f9b2bd44e96ba ;;
      *) fail 'unsupported Inspect target' ;;
    esac
  }
  inspect_target
  preflight_inspect() {
    for dir in "$HOME/.aside" "$INSTALL_DIR" "$HOME/.local" "$LINK_DIR"; do
      [ ! -L "$dir" ] || fail "refusing to install through symlink $dir"
      [ ! -e "$dir" ] || [ -d "$dir" ] || fail "refusing to install through non-directory $dir"
    done
    preflight_binary "$INSPECT_BINARY" "$INSPECT_LINK"
    if [ -f "$INSPECT_BINARY" ]; then
      existing=$(hash_file "$INSPECT_BINARY") || fail 'cannot hash existing Inspect binary'
      [ "$existing" = "$inspect_hash" ] || fail "refusing to replace unrecognized Inspect binary $INSPECT_BINARY"
      [ -x "$INSPECT_BINARY" ] || fail "existing Inspect binary is not executable: $INSPECT_BINARY"
    fi
  }
  # Reuse a validated installed binary. A correct link needs no changes at all.
  preflight_inspect
  if [ -f "$INSPECT_BINARY" ] && [ -L "$INSPECT_LINK" ]; then
    exit 0
  fi
  if [ ! -f "$INSPECT_BINARY" ]; then
    download "$INSPECT_RELEASE/$inspect_asset" "$TMP_DIR/$inspect_asset"
    actual=$(hash_file "$TMP_DIR/$inspect_asset") || fail 'cannot hash Inspect binary'
    [ "$actual" = "$inspect_hash" ] || fail "SHA256 mismatch for $inspect_asset; refusing to install"
  fi
  preflight_inspect
  written=0
  if [ ! -f "$INSPECT_BINARY" ]; then
    # Publish from the same filesystem without replacing an existing file.
    staged=$(mktemp "$INSTALL_DIR/.inspect.XXXXXXXX") || exit 1
    install -m 0755 "$TMP_DIR/$inspect_asset" "$staged" || { rm -f "$staged"; exit 1; }
    ln "$staged" "$INSPECT_BINARY" || { rm -f "$staged"; exit 1; }
    written=1
    if ! rm -f "$staged"; then
      rm -f "$INSPECT_BINARY" || :
      exit 1
    fi
  fi
  # A previously validated link already points at the installed binary.
  if [ -L "$INSPECT_LINK" ] && [ "$(readlink "$INSPECT_LINK")" = "$INSPECT_BINARY" ]; then
    exit 0
  fi
  if ! ln -s "$INSPECT_BINARY" "$INSPECT_LINK"; then
    [ "$written" = 0 ] || rm -f "$INSPECT_BINARY" || :
    exit 1
  fi
)

sample_review() {
  ( : < /dev/tty ) 2>/dev/null || return 0
  printf 'Start an optional live sample review? [y/N] ' > /dev/tty
  IFS= read -r answer < /dev/tty || answer=
  case "$answer" in y|Y|yes|YES|Yes) : ;; *) return 0 ;; esac
  available=
  for agent in claude codex pi; do
    if command -v "$agent" >/dev/null 2>&1; then available="$available $agent"; fi
  done
  if [ -z "$available" ]; then
    printf 'No supported agent CLI found; run a review later with an installed agent.\n' > /dev/tty
    return 0
  fi
  printf 'Choose an installed agent CLI (%s) [Enter to skip]: ' "$available" > /dev/tty
  IFS= read -r agent < /dev/tty || agent=
  case "$agent" in
    claude|codex|pi)
      command -v "$agent" >/dev/null 2>&1 || { printf 'Agent CLI unavailable; skipping.\n' > /dev/tty; return 0; } ;;
    *) printf 'Skipping sample review.\n' > /dev/tty; return 0 ;;
  esac
  prompt='use aside to review https://github.com/cli/cli/pull/14921'
  PATH="$INSTALL_DIR:$PATH" "$agent" "$prompt" < /dev/tty
}

from_source=0
with_inspect=0
for option do
  case "$option" in
    --from-source) [ "$from_source" = 0 ] || fail 'duplicate --from-source'; from_source=1 ;;
    --with-inspect) [ "$with_inspect" = 0 ] || fail 'duplicate --with-inspect'; with_inspect=1 ;;
    *) fail 'usage: install.sh [--from-source] [--with-inspect]' ;;
  esac
done

require uname
require install
require mktemp
require curl
require awk
if command -v sha256sum >/dev/null 2>&1; then
  hash_file() { sha256sum "$1" | awk '{print $1}'; }
else
  require shasum
  hash_file() { shasum -a 256 "$1" | awk '{print $1}'; }
fi
platform=$(target)
TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/aside-install.XXXXXXXX") || fail 'cannot create temporary directory'

# Download the latest verified release by default, even from inside a checkout.
# Building from source requires an explicit choice and the complete Aside tree.
if [ "$from_source" = 1 ]; then
  [ -f "$0" ] || fail 'source install requires install.sh from the Aside checkout'
  root=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
  [ -f "$root/Cargo.toml" ] && [ -f "$root/skills/aside/SKILL.md" ] && [ -f "$root/app/package.json" ] \
    || fail 'source install requires the Aside checkout with app/ and skills/aside/'
  require cargo
  require pnpm
  pnpm -C "$root/app" install --frozen-lockfile || fail 'frontend dependency install failed'
  pnpm -C "$root/app" build || fail 'frontend build failed'
  (cd "$root" && cargo build --locked --release) || fail 'source build failed'
  [ -f "$root/target/release/aside" ] || fail 'source build did not produce aside'
  aside_source="$root/target/release/aside"
else
  require tar
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
  aside_source="$TMP_DIR/aside"
fi

install_verified "$aside_source"
"$BINARY" --version || fail 'installed binary did not run'
"$BINARY" skill || fail 'embedded skill installation failed'
transaction=0
printf 'Installed aside at %s (linked at %s).\n' "$BINARY" "$LINK"
if [ "$with_inspect" = 1 ]; then
  printf 'Inspect v0.1.1 is licensed FSL-1.1-ALv2: %s\n' "$INSPECT_LICENSE"
  if install_inspect; then
    printf 'Installed Inspect at %s (linked at %s).\n' "$INSPECT_BINARY" "$INSPECT_LINK"
  else
    printf 'warning: optional Inspect installation failed; Aside remains installed.\n' >&2
  fi
fi
case ":${PATH:-}:" in *":$LINK_DIR:"*) : ;; *) printf 'Add %s to PATH to use aside from a new terminal.\n' "$LINK_DIR" ;; esac
sample_review
