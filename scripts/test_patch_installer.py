import os
from pathlib import Path
import pty
import tempfile
import unittest

from patch_installer import patch


INSTALLER = """set -u
PRINT_QUIET=0
say() {
    echo "$1"
}
install() {
    local _install_dir="$1"
    say "everything's installed!"
}
download_binary_and_run_installer() {
    install "$FAKE_BIN_DIR"
}
download_binary_and_run_installer "$@" || exit 1
"""

FAKE_CLI = """#!/bin/sh
echo "$1" >> "$FAKE_BIN_DIR/calls"
[ "$1" = whoami ] && exit "${FAKE_WHOAMI_EXIT:-3}"
exit 0
"""


def run_in_terminal(script, env):
    pid, fd = pty.fork()
    if pid == 0:
        os.execve("/bin/sh", ["sh", str(script)], env)
    output = b""
    while True:
        try:
            chunk = os.read(fd, 1024)
        except OSError:
            break
        if not chunk:
            break
        output += chunk
    os.waitpid(pid, 0)
    return output.decode()


class PatchInstallerTests(unittest.TestCase):
    def install(self, **env):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cli = root / "cloudthinker"
            cli.write_text(FAKE_CLI)
            cli.chmod(0o755)
            script = root / "installer.sh"
            script.write_text(patch(INSTALLER))
            output = run_in_terminal(
                script, {"PATH": "/usr/bin:/bin", "FAKE_BIN_DIR": directory, **env}
            )
            calls = root / "calls"
            return (calls.read_text().split() if calls.exists() else []), output

    def test_a_fresh_install_opens_the_login(self):
        calls, output = self.install()

        self.assertEqual(calls, ["whoami", "login"])
        self.assertIn("Run cloudthinker to start the agent.", output)

    def test_an_update_or_an_existing_login_skips_it(self):
        for env in (
            {"CARGO_DIST_FORCE_INSTALL_DIR": "/opt"},
            {"CI": "true"},
            {"CLOUDTHINKER_CLI_NO_LOGIN": "1"},
        ):
            self.assertEqual(self.install(**env)[0], [], env)
        self.assertEqual(self.install(FAKE_WHOAMI_EXIT="0")[0], ["whoami"])

    def test_a_changed_installer_fails_the_patch(self):
        with self.assertRaises(ValueError):
            patch(INSTALLER.replace("everything's installed!", "done"))
        with self.assertRaises(ValueError):
            patch(patch(INSTALLER))


if __name__ == "__main__":
    unittest.main()
