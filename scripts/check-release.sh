#!/usr/bin/env bash
set -euo pipefail

REPO="cloudthinker-ai/cloudthinker-cli"
TAG="${TAG:-latest}"

if [ "$TAG" = "latest" ]; then
  api="https://api.github.com/repos/$REPO/releases/latest"
else
  api="https://api.github.com/repos/$REPO/releases/tags/$TAG"
fi

echo ">> $api (unauthenticated)"
release="$(curl -fsSL -H 'Accept: application/vnd.github+json' "$api")" || {
  echo "error: no public release at $api" >&2
  exit 1
}
tag="$(printf '%s' "$release" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_name"])')"
mapfile -t assets < <(printf '%s' "$release" | python3 -c '
import json, sys
for asset in json.load(sys.stdin)["assets"]:
    print(asset["name"])
')
echo ">> release $tag carries ${#assets[@]} assets"

expected=(
  cloudthinker-cli-aarch64-apple-darwin.tar.xz
  cloudthinker-cli-x86_64-apple-darwin.tar.xz
  cloudthinker-cli-aarch64-unknown-linux-gnu.tar.xz
  cloudthinker-cli-x86_64-unknown-linux-gnu.tar.xz
  cloudthinker-cli-x86_64-pc-windows-msvc.zip
  cloudthinker-cli-installer.sh
  cloudthinker-cli-installer.ps1
  sha256.sum
  cloudthinker-agent-aarch64-apple-darwin.tar.gz
  cloudthinker-agent-aarch64-apple-darwin.tar.gz.sha256
  cloudthinker-agent-x86_64-apple-darwin.tar.gz
  cloudthinker-agent-x86_64-apple-darwin.tar.gz.sha256
  cloudthinker-agent-aarch64-unknown-linux-gnu.tar.gz
  cloudthinker-agent-aarch64-unknown-linux-gnu.tar.gz.sha256
  cloudthinker-agent-x86_64-unknown-linux-gnu.tar.gz
  cloudthinker-agent-x86_64-unknown-linux-gnu.tar.gz.sha256
  cloudthinker-agent-sha256.sum
)
missing=0
for name in "${expected[@]}"; do
  if grep -qx -- "$name" <<<"$(printf '%s\n' "${assets[@]}")"; then
    continue
  fi
  echo "error: missing asset $name" >&2
  missing=1
done
if grep -q '^source\.tar\.gz' <<<"$(printf '%s\n' "${assets[@]}")"; then
  echo "error: the release carries a source tarball; set source-tarball = false" >&2
  missing=1
fi
[ "$missing" -eq 0 ] || exit 1

echo ">> every asset downloads without a token"
for name in "${expected[@]}"; do
  url="https://github.com/$REPO/releases/download/$tag/$name"
  curl -fsSIL -o /dev/null "$url" || { echo "error: $url is not public" >&2; exit 1; }
done
curl -fsSIL -o /dev/null "https://github.com/$REPO/releases/latest/download/cloudthinker-cli-installer.sh" || {
  echo "error: releases/latest/download does not resolve" >&2
  exit 1
}
echo ">> OK: $tag is public on $REPO with every asset and no source tarball"
