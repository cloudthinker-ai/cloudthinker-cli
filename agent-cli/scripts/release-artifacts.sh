#!/usr/bin/env bash
set -euo pipefail

AGENT_CLI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MIRROR_ROOT="$(cd "$AGENT_CLI_DIR/.." && pwd)"
CARGO_TOML="$MIRROR_ROOT/crates/cloudthinker-cli/Cargo.toml"

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  if [ ! -f "$CARGO_TOML" ]; then
    echo "error: no version argument and no $CARGO_TOML; this path resolves only in the cloudthinker-cli release mirror" >&2
    exit 1
  fi
  VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$CARGO_TOML" | head -1)"
fi
[ -n "$VERSION" ] || {
  echo "error: could not read the CLI version from $CARGO_TOML" >&2
  exit 1
}

BUN_VERSION="1.4.2"

if [ -x "$HOME/.bun/bin/bun" ]; then
  BUN="$HOME/.bun/bin/bun"
elif command -v bun > /dev/null; then
  BUN="$(command -v bun)"
else
  curl -fsSL https://bun.sh/install | bash -s "bun-v$BUN_VERSION"
  BUN="$HOME/.bun/bin/bun"
fi

INSTALLED_BUN="$("$BUN" --version)"
if [ "$INSTALLED_BUN" != "$BUN_VERSION" ]; then
  echo "error: bun $INSTALLED_BUN at $BUN, but the build expects bun $BUN_VERSION" >&2
  exit 1
fi

if ! command -v pnpm > /dev/null; then
  corepack enable pnpm
fi

cd "$AGENT_CLI_DIR"
pnpm install --frozen-lockfile
BUN="$BUN" bash packages/agent/scripts/build.sh "$VERSION"
