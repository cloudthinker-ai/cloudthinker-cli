import os
import pty
import select
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

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


def run_in_terminal(script, env, answer="\n"):
    pid, fd = pty.fork()
    if pid == 0:
        os.execve("/bin/sh", ["sh", str(script)], env)
    output = b""
    deadline = time.monotonic() + 12
    answered = False
    while True:
        if time.monotonic() > deadline:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            raise AssertionError(f"Installer timed out: {output.decode()}")
        if not select.select([fd], [], [], 0.1)[0]:
            continue
        try:
            chunk = os.read(fd, 1024)
        except OSError:
            break
        if not chunk:
            break
        output += chunk
        if b"[y/N]" in output and not answered:
            os.write(fd, answer.encode())
            answered = True
    os.close(fd)
    _, status = os.waitpid(pid, 0)
    if status != 0:
        raise AssertionError(f"Installer failed: {output.decode()}")
    return output.decode()


class PatchInstallerTests(unittest.TestCase):
    def alias_install(
        self,
        answer="\n",
        profile="",
        repeat=False,
        prepare=None,
        inspect=None,
        terminal=True,
        quiet=False,
        resolve=True,
        configure_environment=None,
        **env,
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cli = root / "cloudthinker"
            cli.write_text(FAKE_CLI)
            cli.chmod(0o755)
            rc = root / ".bashrc"
            prefix = f"export PATH='{directory}:/usr/bin:/bin'\n"
            rc.write_text(prefix + profile)
            script = root / "installer.sh"
            script.write_text(
                patch(INSTALLER.replace("PRINT_QUIET=0", f"PRINT_QUIET={int(quiet)}"))
            )
            environment = {
                "PATH": f"{directory}:/usr/bin:/bin",
                "HOME": directory,
                "SHELL": "/bin/bash",
                "FAKE_BIN_DIR": directory,
                **env,
                **(configure_environment(root) if configure_environment else {}),
            }
            if prepare:
                prepare(root)
            if terminal:
                output = run_in_terminal(script, environment, answer)
            else:
                output = subprocess.run(
                    ["/bin/sh", str(script)],
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=5,
                ).stdout
            if repeat:
                output += run_in_terminal(script, environment, answer)
            saved = rc.read_text().removeprefix(prefix) if rc.is_file() else ""
            resolved = (
                subprocess.run(
                    ["/bin/bash", "-ic", "type ct"],
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=5,
                ).stdout
                if resolve
                else ""
            )
            if inspect:
                inspect(root, environment)
            return output, saved, resolved

    def test_existing_command_is_preserved(self):
        def prepare(root):
            command = root / "ct"
            command.write_text("#!/bin/sh\necho existing\n")
            command.chmod(0o755)

        output, profile, _ = self.alias_install("y\n", prepare=prepare)
        self.assertEqual(profile, "")
        self.assertIn("already exists", output)
        self.assertNotIn("[y/N]", output)

    def test_bash_login_profiles_are_preserved_and_receive_the_alias(self):
        for filename in (".bash_profile", ".bash_login", ".profile"):

            def prepare(root, filename=filename):
                (root / filename).write_text("export ALIAS_INSTALL_TEST=1\n")

            def inspect(root, env, filename=filename):
                saved = (root / filename).read_text()
                self.assertIn("export ALIAS_INSTALL_TEST=1", saved)
                self.assertEqual(saved.count('alias ct="cloudthinker"'), 1)

            output, profile, _ = self.alias_install("y\n", prepare=prepare, inspect=inspect)
            self.assertIn("alias ct=", profile)
            self.assertIn("Added ct", output)

    def test_bash_login_profile_conflicts_are_preserved(self):
        def prepare(root):
            (root / ".bash_profile").write_text("alias ct='echo existing'\n")

        output, profile, _ = self.alias_install("y\n", prepare=prepare)
        self.assertEqual(profile, "")
        self.assertIn("already exists", output)

    def test_quiet_or_nonterminal_installs_do_not_prompt(self):
        for options in ({"quiet": True}, {"terminal": False}):
            output, profile, _ = self.alias_install("y\n", **options)
            self.assertEqual(profile, "")
            self.assertNotIn("[y/N]", output)

    def test_unsupported_or_missing_shell_does_not_block_login(self):
        for shell in ("/bin/sh", "/not-installed/bash"):
            output, profile, _ = self.alias_install("y\n", SHELL=shell)
            self.assertEqual(profile, "")
            self.assertIn("Run cloudthinker", output)
            self.assertNotIn("[y/N]", output)

    def test_profile_write_failure_keeps_installation_successful(self):
        def prepare(root):
            (root / ".bash_profile").mkdir()

        output, _, _ = self.alias_install("y\n", prepare=prepare)
        self.assertIn("Could not update", output)
        self.assertIn("Run cloudthinker", output)

    def test_a_stalled_shell_check_is_bounded(self):
        output, profile, _ = self.alias_install("y\n", profile="sleep 6\n", resolve=False)
        self.assertEqual(profile, "sleep 6\n")
        self.assertIn("Could not check your shell", output)
        self.assertIn("Run cloudthinker", output)

    def test_zsh_and_fish_aliases_work_with_custom_config_paths(self):
        shells = {
            "zsh": os.environ.get("TEST_ZSH_BIN") or shutil.which("zsh"),
            "fish": os.environ.get("TEST_FISH_BIN") or shutil.which("fish"),
        }
        for name, shell in shells.items():
            with self.subTest(shell=name):
                if not shell:
                    self.skipTest(f"{name} is not installed")

                def prepare(root, name=name):
                    custom = root / "custom"
                    custom.mkdir()
                    if name == "zsh":
                        (custom / ".zshrc").write_text("export ALIAS_INSTALL_TEST=1\n")
                        (root / ".zshenv").write_text('ZDOTDIR="$HOME/custom"\n')

                def inspect(root, env, name=name, shell=shell):
                    profile = root / "custom" / (".zshrc" if name == "zsh" else "fish/config.fish")
                    self.assertEqual(profile.read_text().count('alias ct="cloudthinker"'), 1)
                    result = subprocess.run(
                        [shell, "-ic", "type ct"],
                        env=env,
                        capture_output=True,
                        text=True,
                        timeout=5,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("cloudthinker", result.stdout)

                def environment(root):
                    return {
                        "XDG_CONFIG_HOME": str(root / "custom"),
                        "LD_LIBRARY_PATH": os.environ.get("LD_LIBRARY_PATH", ""),
                        **(
                            {"fish_function_path": os.environ["fish_function_path"]}
                            if "fish_function_path" in os.environ
                            else {}
                        ),
                    }

                self.alias_install(
                    "y\n",
                    repeat=True,
                    prepare=prepare,
                    inspect=inspect,
                    SHELL=shell,
                    configure_environment=environment,
                )

    def test_accept_adds_a_working_persistent_alias_once(self):
        output, profile, resolved = self.alias_install("y\n", repeat=True)
        self.assertEqual(profile.count("alias ct="), 1)
        self.assertIn("cloudthinker", resolved)
        self.assertEqual(output.count("[y/N]"), 1)
        self.assertIn("Open a new terminal", output)

    def test_decline_and_default_are_remembered(self):
        for answer in ("n\n", "\n"):
            output, profile, resolved = self.alias_install(answer, repeat=True)
            self.assertEqual(profile, "")
            self.assertEqual(resolved, "")
            self.assertEqual(output.count("[y/N]"), 1)

    def test_existing_shell_alias_or_function_is_preserved(self):
        for profile in ("alias ct='echo existing'\n", "ct() { echo existing; }\n"):
            output, saved, _ = self.alias_install("y\n", profile=profile)
            self.assertEqual(saved, profile)
            self.assertIn("already exists", output)
            self.assertNotIn("[y/N]", output)

    def test_alias_setup_is_independent_of_login(self):
        for env in ({"CLOUDTHINKER_CLI_NO_LOGIN": "1"}, {"FAKE_WHOAMI_EXIT": "0"}):
            output, profile, _ = self.alias_install("yes\n", **env)
            self.assertIn("alias ct=", profile)
            self.assertIn("[y/N]", output)

    def test_unattended_installs_leave_profiles_alone(self):
        for env in ({"CI": "true"}, {"CARGO_DIST_FORCE_INSTALL_DIR": "/opt"}):
            output, profile, _ = self.alias_install("y\n", **env)
            self.assertEqual(profile, "")
            self.assertNotIn("[y/N]", output)

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
