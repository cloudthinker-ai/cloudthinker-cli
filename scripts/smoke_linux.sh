#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: smoke_linux.sh <distrib-dir> <image>..." >&2
  echo "Installs the CLI from <distrib-dir> with its shell installer in each image," >&2
  echo "then installs and starts the agent the CLI picks, against a local stub." >&2
  exit 2
}

[ "$#" -ge 2 ] || usage
DIST="$(cd "$1" && pwd)"
shift
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="$DIST/cloudthinker-cli-installer.sh"
[ -f "$INSTALLER" ] || { echo "missing $INSTALLER" >&2; exit 2; }

coproc SERVER { exec python3 -I "$SCRIPT_DIR/smoke_linux_server.py" "$DIST"; }
SERVER_PID="$SERVER_PID"
trap 'kill "$SERVER_PID" 2>/dev/null || true' EXIT
read -r PORT <&"${SERVER[0]}"

INNER='
set -eu
if command -v apk >/dev/null 2>&1; then
  apk add --no-cache -q curl xz libstdc++ >/dev/null
elif command -v apt-get >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq >/dev/null && apt-get install -y -qq curl ca-certificates xz-utils >/dev/null
elif command -v dnf >/dev/null 2>&1; then
  dnf install -y -q --allowerasing curl tar xz gzip >/dev/null
elif command -v yum >/dev/null 2>&1; then
  yum install -y -q curl tar xz gzip >/dev/null
fi
export HOME=/root CI=1 CLOUDTHINKER_CLI_NO_LOGIN=1 CLOUDTHINKER_CLI_NO_MODIFY_PATH=1
export CLOUDTHINKER_CLI_INSTALL_DIR=/opt/cloudthinker
export CLOUDTHINKER_CLI_DOWNLOAD_URL="http://127.0.0.1:$PORT"
sh /dist/cloudthinker-cli-installer.sh
echo "cli: $(/opt/cloudthinker/cloudthinker --version)"
export CLOUDTHINKER_URL="http://127.0.0.1:$PORT" CLOUDTHINKER_TOKEN=smoke
export CLOUDTHINKER_NO_UPDATE_CHECK=1 CLOUDTHINKER_RELEASE_URL=/dist
echo "agent: $(/opt/cloudthinker/cloudthinker agent --version </dev/null)"
'

failed=0
for image in "$@"; do
  log="$(mktemp)"
  docker pull -q "$image" >/dev/null
  if docker run --rm --network host -e PORT="$PORT" -v "$DIST:/dist:ro" "$image" sh -c "$INNER" >"$log" 2>&1; then
    printf 'PASS %-22s %s\n' "$image" "$(grep -E '^(cli|agent):' "$log" | tr '\n' ' ')"
  else
    failed=1
    printf 'FAIL %-22s\n' "$image"
    tail -n 12 "$log" | sed 's/^/     /'
  fi
  rm -f "$log"
done
exit "$failed"
