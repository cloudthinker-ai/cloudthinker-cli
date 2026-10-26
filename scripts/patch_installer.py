import sys
from pathlib import Path

INSTALLED = """    say "everything's installed!"
"""

RECORD_BIN = """    CLOUDTHINKER_INSTALLED_BIN="$_install_dir/cloudthinker"
"""

RUN = """download_binary_and_run_installer "$@" || exit 1
"""

FIRST_RUN = r"""
cloudthinker_offer_alias() (
    [ -n "${HOME:-}" ] && [ -n "${SHELL:-}" ] || return 0
    _config="${XDG_CONFIG_HOME:-$HOME/.config}"
    _state="$_config/cloudthinker/install-ct-alias"
    [ ! -e "$_state" ] || return 0
    _login=
    _jobs=
    case "${SHELL##*/}" in
        bash)
            _profile="$HOME/.bashrc"
            _login="$HOME/.bash_profile"
            for _candidate in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
                if [ -f "$_candidate" ]; then _login="$_candidate"; break; fi
            done
            _check='command -v ct >/dev/null 2>&1 && exit 42; if [ -f "$_CLOUDTHINKER_LOGIN_PROFILE" ]; then . "$_CLOUDTHINKER_LOGIN_PROFILE"; fi; command -v ct >/dev/null 2>&1 && exit 42; exit 0'
            ;;
        zsh) _jobs=+m; _profile="${ZDOTDIR:-$HOME}/.zshrc"; _check='command -v ct >/dev/null 2>&1 && exit 42; printf "%s\n" "${ZDOTDIR:-$HOME}/.zshrc" >&3; exit 0' ;;
        fish) _profile="$_config/fish/config.fish"; _check='if type -q ct; exit 42; end; exit 0' ;;
        *) say 'To use ct, add a shortcut for cloudthinker in your shell settings.'; return 0 ;;
    esac
    umask 077
    _profile_file=$(mktemp "${TMPDIR:-/tmp}/cloudthinker-alias.XXXXXX") || {
        say 'Could not check your shell for ct. Continue with cloudthinker.'
        return 0
    }
    trap 'rm -f "$_profile_file"' 0
    _CLOUDTHINKER_LOGIN_PROFILE="$_login" "$SHELL" $_jobs -ic "$_check" </dev/null >/dev/null 2>&1 3>"$_profile_file" &
    _probe=$!
    (sleep 5; kill -KILL "$_probe" 2>/dev/null) &
    _watchdog=$!
    _result=0
    wait "$_probe" 2>/dev/null || _result=$?
    kill "$_watchdog" 2>/dev/null || :
    wait "$_watchdog" 2>/dev/null || :
    case "$_result" in
        0) ;;
        42) say 'ct already exists in your shell. Keeping your current command.'; return 0 ;;
        *) say 'Could not check your shell for ct. Continue with cloudthinker.'; return 0 ;;
    esac
    if [ "${SHELL##*/}" = zsh ]; then
        IFS= read -r _profile <"$_profile_file" || {
            say 'Could not find your shell settings. Continue with cloudthinker.'
            return 0
        }
    fi
    if ! mkdir -p "${_state%/*}"; then
        say 'Could not save your shortcut preference. Continue with cloudthinker.'
        return 0
    fi
    printf '\nUse `ct` as a shortcut for `cloudthinker`? [y/N] ' >/dev/tty
    _answer=
    IFS= read -r _answer </dev/tty || return 0
    case "$_answer" in
        y|Y|yes|Yes|YES)
            for _target in "$_profile" "$_login"; do
                [ -n "$_target" ] || continue
                if { [ -e "$_target" ] && { [ ! -f "$_target" ] || [ ! -w "$_target" ]; }; } || ! mkdir -p "${_target%/*}"; then
                    say "Could not update $_target. Continue with cloudthinker."
                    return 0
                fi
            done
            for _target in "$_profile" "$_login"; do
                [ -n "$_target" ] || continue
                if ! printf '\nalias ct="cloudthinker"\n' >>"$_target"; then
                    say "Could not update $_target. Continue with cloudthinker."
                    return 0
                fi
                say "Added ct to $_target."
            done
            say 'Open a new terminal to use it. Both ct and cloudthinker are available.'
            _choice=accepted
            ;;
        *) say 'Keeping cloudthinker as your command.'; _choice=declined ;;
    esac
    if ! printf '%s\n' "$_choice" >"$_state"; then
        say 'Could not save your shortcut preference. A reinstall may ask again.'
    fi
    return 0
)

cloudthinker_first_run() {
    _bin="${CLOUDTHINKER_INSTALLED_BIN:-}"
    [ -n "$_bin" ] && [ -x "$_bin" ] || return 0
    [ -z "${CARGO_DIST_FORCE_INSTALL_DIR:-}" ] || return 0
    [ -z "${CI:-}" ] || return 0
    [ "$PRINT_QUIET" = "0" ] || return 0
    [ -t 1 ] || return 0
    (exec </dev/tty) 2>/dev/null || return 0
    cloudthinker_offer_alias
    [ "${CLOUDTHINKER_CLI_NO_LOGIN:-0}" = "0" ] || return 0
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
