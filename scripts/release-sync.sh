#!/usr/bin/env bash
# Publish the monorepo's cli/ into the private source repo
# github.com/cloudthinker-ai/cloudthinker-cli-src, whose release workflow publishes
# the public releases to github.com/cloudthinker-ai/cloudthinker-cli.
#
# The GitLab monorepo is the source of truth; this pushes a one-directional
# snapshot so cargo-dist can cross-build + release it. Never edit the GitHub repo
# by hand — it is a publish mirror.
#
# GitButler-safe: all work happens in a throwaway clone; the monorepo's own git is
# only ever READ (rev-parse), never mutated.
#
# Requires: gh authenticated with an account that has push access to the repo
# (e.g. `gh auth switch --user duc-cloudthinker`). Run: `make -C cli release-sync`.
set -euo pipefail

REPO="cloudthinker-ai/cloudthinker-cli-src"
CLI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENT_CLI_DIR="$(cd "$CLI_DIR/../agent-cli" && pwd)"
MONO_SHA="$(git -C "$CLI_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)"

command -v gh >/dev/null || { echo "error: gh CLI required" >&2; exit 1; }
command -v rsync >/dev/null || { echo "error: rsync required" >&2; exit 1; }

dist_config="$CLI_DIR/dist-workspace.toml"
grep -qx 'github-releases-repo = "cloudthinker-ai/cloudthinker-cli"' "$dist_config" || {
  echo "error: $dist_config must set github-releases-repo = \"cloudthinker-ai/cloudthinker-cli\"" >&2
  exit 1
}
grep -qx 'source-tarball = false' "$dist_config" || {
  echo "error: $dist_config must set source-tarball = false" >&2
  exit 1
}
grep -q 'GH_TOKEN: \${{ secrets.GH_RELEASES_TOKEN }}' "$CLI_DIR/.github/workflows/release.yml" || {
  echo "error: release.yml must create the release with secrets.GH_RELEASES_TOKEN; rerun dist generate --mode ci" >&2
  exit 1
}

# Preflight: the active gh account must be able to push to the release repo.
push=$(gh api "/repos/$REPO" --jq '.permissions.push' 2>/dev/null || echo false)
if [ "$push" != "true" ]; then
  echo "error: active gh account lacks push access to $REPO" >&2
  echo "  fix: gh auth switch --user <account-with-access>   # e.g. duc-cloudthinker" >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo ">> clone $REPO"
gh repo clone "$REPO" "$work/repo" -- --quiet

echo ">> mirror cli/ -> clone (drop build dirs, protect the clone's .git)"
rsync -a --delete \
  --exclude='.git/' --exclude='target/' --exclude='.gen/' --exclude='.DS_Store' \
  "$CLI_DIR"/ "$work/repo"/

echo ">> mirror agent-cli/ -> clone/agent-cli (drop node_modules and build dirs)"
rsync -a --delete \
  --exclude='node_modules/' --exclude='dist/' --exclude='.gen/' --exclude='.DS_Store' \
  "$AGENT_CLI_DIR"/ "$work/repo/agent-cli"/

cd "$work/repo"
git add -A
if git diff --cached --quiet; then
  echo ">> already in sync — nothing to push"
  exit 0
fi

echo ">> changes to publish:"
git diff --cached --stat | sed 's/^/     /'
git -c user.name="cloudthinker-release-bot" \
    -c user.email="release-bot@cloudthinker.ai" \
    commit -q -m "sync: cli/ from monorepo @ $MONO_SHA"
git push -q origin HEAD
echo ">> pushed to github.com/$REPO (source monorepo @ $MONO_SHA)"
echo ">> next: bump crates/cloudthinker-cli/Cargo.toml version, tag vX.Y.Z, push the tag to $REPO; the release lands on cloudthinker-ai/cloudthinker-cli"
