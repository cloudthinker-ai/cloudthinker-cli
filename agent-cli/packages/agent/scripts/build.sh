#!/usr/bin/env bash
set -euo pipefail

PKG_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUN="${BUN:-$HOME/.bun/bin/bun}"
ALL_TRIPLES="aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu"

usage() {
  cat <<'USAGE'
build.sh [<version>] [--out <dir>] [--target <triple>] [--host]

  <version>          Defaults to the version in packages/agent/package.json.
  --out <dir>        Where the tarballs land. Default: packages/agent/dist.
  --target <triple>  Build one Rust target triple only. Repeatable.
  --host             Build only this machine's triple.

Triples: aarch64-apple-darwin x86_64-apple-darwin
         aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu
USAGE
}

bun_target() {
  case "$1" in
    aarch64-apple-darwin) echo bun-darwin-arm64 ;;
    x86_64-apple-darwin) echo bun-darwin-x64 ;;
    aarch64-unknown-linux-gnu) echo bun-linux-arm64 ;;
    x86_64-unknown-linux-gnu) echo bun-linux-x64 ;;
    *)
      echo "error: unknown target triple '$1'" >&2
      return 1
      ;;
  esac
}

host_triple() {
  local machine
  machine="$(uname -m)"
  case "$(uname -s)/$machine" in
    Darwin/arm64) echo aarch64-apple-darwin ;;
    Darwin/x86_64) echo x86_64-apple-darwin ;;
    Linux/aarch64) echo aarch64-unknown-linux-gnu ;;
    Linux/x86_64) echo x86_64-unknown-linux-gnu ;;
    *)
      echo "error: no release triple for $(uname -s)/$machine" >&2
      return 1
      ;;
  esac
}

VERSION=""
OUT="$PKG_DIR/dist"
TRIPLES=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out)
      OUT="$2"
      shift 2
      ;;
    --target)
      TRIPLES="$TRIPLES $2"
      shift 2
      ;;
    --host)
      TRIPLES="$TRIPLES $(host_triple)"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*)
      usage >&2
      exit 2
      ;;
    *)
      VERSION="$1"
      shift
      ;;
  esac
done
[ -n "$TRIPLES" ] || TRIPLES="$ALL_TRIPLES"

command -v "$BUN" > /dev/null || {
  echo "error: bun not found at '$BUN'; set BUN to its path" >&2
  exit 1
}

PI_ROOT="$(cd "$PKG_DIR/node_modules/@earendil-works/pi-coding-agent" && pwd -P)"
PHOTON="$(cd "$PI_ROOT/../.." && pwd -P)/@silvia-odwyer/photon-node/photon_rs_bg.wasm"
if [ -z "$VERSION" ]; then
  VERSION="$(node -p "require('$PKG_DIR/package.json').version")"
fi
PI_VERSION="$(node -p "require('$PI_ROOT/package.json').version")"
PI_REPOSITORY="$(node -p "
  const r = require('$PI_ROOT/package.json').repository;
  (typeof r === 'string' ? r : r.url).replace(/^git\+/, '').replace(/\.git\$/, '')
")"
PI_AUTHOR="$(node -p "require('$PI_ROOT/package.json').author")"
SOURCE_REVISION="$PKG_DIR/../../.source-revision"
if [ -f "$SOURCE_REVISION" ]; then
  BUILD_ID="$(<"$SOURCE_REVISION")"
  if ! [[ "$BUILD_ID" =~ ^[0-9a-f]{40}$ ]]; then
    echo "error: invalid monorepo source revision" >&2
    exit 1
  fi
else
  BUILD_ID="$(git -C "$PKG_DIR" rev-parse HEAD)"
fi
if [ -n "$(git -C "$PKG_DIR" status --porcelain -- . ../pi)" ]; then
  BUILD_ID="$BUILD_ID-dirty"
fi

mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
assets=()

for triple in $TRIPLES; do
  target="$(bun_target "$triple")"
  root="$STAGE/$triple"
  dir="$root/cloudthinker-agent"
  mkdir -p "$dir/theme" "$dir/assets" "$dir/export-html/vendor"

  (cd "$PKG_DIR" && "$BUN" build --compile --minify --keep-names --bytecode --format=esm --no-compile-autoload-bunfig \
    --define "__CT_BUILD_ID__=\"$BUILD_ID\"" \
    "--target=$target" src/main.ts --outfile "$dir/cloudthinker-agent")
  chmod 0755 "$dir/cloudthinker-agent"

  cp "$PI_ROOT"/dist/modes/interactive/theme/*.json "$dir/theme/"
  cp "$PKG_DIR"/themes/*.json "$dir/theme/"
  cp "$PI_ROOT"/dist/modes/interactive/assets/*.png "$dir/assets/"
  cp "$PI_ROOT"/dist/core/export-html/template.html "$dir/export-html/"
  cp "$PI_ROOT"/dist/core/export-html/template.css "$dir/export-html/"
  cp "$PI_ROOT"/dist/core/export-html/vendor/*.js "$dir/export-html/vendor/"
  node -e '
    const { readFileSync, writeFileSync } = require("node:fs");
    const [source, target] = process.argv.slice(1);
    const row = "<div class=\"info-item\"><span class=\"info-label\">Cost:</span><span class=\"info-value\">$${totalCost.toFixed(3)}</span></div>";
    const script = readFileSync(source, "utf8");
    if (!script.includes(row)) {
      console.error("error: pi export template no longer prints the dollar cost row; drop this patch");
      process.exit(1);
    }
    writeFileSync(target, script.split(row).join(""), "utf8");
  ' "$PI_ROOT/dist/core/export-html/template.js" "$dir/export-html/template.js"
  cp "$PHOTON" "$dir/"
  cp -r "$PI_ROOT/docs" "$PI_ROOT/examples" "$dir/"

  cat > "$dir/package.json" <<JSON
{
	"name": "@cloudthinker/agent",
	"version": "$VERSION",
	"type": "module",
	"private": true,
	"piVersion": "$PI_VERSION",
	"piRepository": "$PI_REPOSITORY",
	"buildId": "$BUILD_ID",
	"piConfig": { "name": "cloudthinker", "configDir": ".cloudthinker" }
}
JSON

  cat > "$dir/NOTICE" <<NOTICE
cloudthinker-agent is built on pi ($PI_REPOSITORY) by $PI_AUTHOR, MIT License.

MIT License

Copyright (c) $PI_AUTHOR

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
NOTICE
	cat "$PKG_DIR/node_modules/@tintinweb/pi-subagents/LICENSE" >> "$dir/NOTICE"

  node "$PKG_DIR/scripts/validate-assets.ts" "$dir" "$PI_ROOT"
  asset="cloudthinker-agent-$triple.tar.gz"
  assets+=("$asset")
  tar -czf "$OUT/$asset" -C "$root" cloudthinker-agent
  (cd "$OUT" && sha256sum "$asset" > "$asset.sha256")
  echo "built $OUT/$asset"
done

(cd "$OUT" && sha256sum "${assets[@]}" > cloudthinker-agent-sha256.sum)
