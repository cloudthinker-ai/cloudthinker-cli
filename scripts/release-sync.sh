#!/usr/bin/env bash
# Publish the monorepo's cli/ into the public source repo
# github.com/cloudthinker-ai/cloudthinker-cli, whose release workflow publishes
# the GitHub Releases on the same repo.
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

REPO="cloudthinker-ai/cloudthinker-cli"
CLI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MONO_ROOT="$(git -C "$CLI_DIR" rev-parse --show-toplevel)"
MONO_SHA="$(git -C "$CLI_DIR" rev-parse HEAD)"

python3 "$CLI_DIR/scripts/changelog.py" pending
if [ -n "$(git -C "$CLI_DIR" status --porcelain -- . ../agent-cli)" ]; then
  echo "error: commit CLI and agent changes before release-sync" >&2
  exit 1
fi

command -v gh >/dev/null || { echo "error: gh CLI required" >&2; exit 1; }
command -v rsync >/dev/null || { echo "error: rsync required" >&2; exit 1; }

dist_config="$CLI_DIR/dist-workspace.toml"
if grep -q '^github-releases-repo' "$dist_config"; then
  echo "error: $dist_config must not set github-releases-repo; releases publish on $REPO itself" >&2
  exit 1
fi
grep -qx 'source-tarball = false' "$dist_config" || {
  echo "error: $dist_config must set source-tarball = false" >&2
  exit 1
}
if grep -q 'GH_RELEASES_TOKEN' "$CLI_DIR/.github/workflows/release.yml"; then
  echo "error: release.yml must create the release with the runner GITHUB_TOKEN, not GH_RELEASES_TOKEN" >&2
  exit 1
fi

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

echo ">> export the committed cli/ and agent-cli/ trees at $MONO_SHA"
mkdir "$work/src"
git -C "$MONO_ROOT" archive --format=tar "$MONO_SHA" -- cli agent-cli \
  ':(exclude,glob)**/AGENTS.md' ':(exclude,glob)**/CLAUDE.md' | tar -x -C "$work/src"

echo ">> mirror cli/ -> clone (protect the clone's .git)"
rsync -a --delete --exclude='.git/' "$work/src/cli"/ "$work/repo"/

echo ">> mirror agent-cli/ -> clone/agent-cli"
rsync -a --delete "$work/src/agent-cli"/ "$work/repo/agent-cli"/
printf '%s\n' "$MONO_SHA" > "$work/repo/agent-cli/.source-revision"

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
echo ">> next: bump crates/cloudthinker-cli/Cargo.toml version, tag vX.Y.Z, push the tag to $REPO; the release lands on the same repo"
