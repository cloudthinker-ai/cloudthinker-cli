import sys
from pathlib import Path


INSTALLED = """    say "everything's installed!"
"""

RECORD_BIN = """    CLOUDTHINKER_INSTALLED_BIN="$_install_dir/cloudthinker"
"""

RUN = """download_binary_and_run_installer "$@" || exit 1
"""

FIRST_RUN = """
cloudthinker_first_run() {
    _bin="${CLOUDTHINKER_INSTALLED_BIN:-}"
    [ -n "$_bin" ] && [ -x "$_bin" ] || return 0
    [ -z "${CARGO_DIST_FORCE_INSTALL_DIR:-}" ] || return 0
    [ -z "${CI:-}" ] || return 0
    [ "${CLOUDTHINKER_CLI_NO_LOGIN:-0}" = "0" ] || return 0
    [ "$PRINT_QUIET" = "0" ] || return 0
    [ -t 1 ] || return 0
    (exec </dev/tty) 2>/dev/null || return 0
    "$_bin" whoami </dev/null >/dev/null 2>&1 && return 0
    if "$_bin" login </dev/tty; then
        say ""
        say "Run cloudthinker to start the agent."
    fi
    return 0
}

cloudthinker_first_run
"""


def patch(text):
    if "cloudthinker_first_run" in text:
        raise ValueError("installer is already patched")
    for anchor in (INSTALLED, RUN):
        if text.count(anchor) != 1:
            raise ValueError(f"installer anchor not found exactly once: {anchor.strip()}")
    if not text.endswith(RUN):
        raise ValueError("installer no longer ends with its run line")
    return text.replace(INSTALLED, INSTALLED + RECORD_BIN) + FIRST_RUN


def main(argv):
    if len(argv) != 2:
        print("usage: patch_installer.py <cloudthinker-cli-installer.sh>", file=sys.stderr)
        return 2
    path = Path(argv[1])
    try:
        path.write_text(patch(path.read_text()))
    except ValueError as error:
        print(f"patch_installer: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
