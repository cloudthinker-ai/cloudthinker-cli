import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class LocalLauncherTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = self.root / "source checkout"
        self.backend = self.root / "backend"
        self.target = self.root / "target"
        self.backend.mkdir()
        self.target.mkdir()
        self.script = self.source / "cli/scripts/cloudthinker-local"
        self.script.parent.mkdir(parents=True)
        shutil.copyfile(Path(__file__).with_name("cloudthinker-local"), self.script)
        self.env = {
            k: v for k, v in os.environ.items()
            if not k.startswith(("CLOUDTHINKER_", "CYBER_", "CT_CLI_"))
        }
        self.env.update({
            "CARGO_HOME": str(self.root / "cargo"),
            "CLOUDTHINKER_LOCAL_SLOT": str(self.backend),
            "CLOUDTHINKER_TARGET_SLOT": str(self.target),
            "CLOUDTHINKER_TARGET_EMAIL": "owner@example.test",
        })
        self.executable(self.root / "cargo/bin/rustup", "#!/bin/sh\nexit 1\n")
        self.executable(self.root / "cargo/bin/cargo", """#!/bin/sh
case "$*" in
  *'env --json'*) printf '{"backend_url":"http://backend.test"}\n' ;;
esac
""")
        self.helper = self.source / ".agents/skills/cloudthinker-cli-kit/scripts/mint_token.sh"
        self.executable(self.helper, """#!/bin/sh
case "$CLOUDTHINKER_LOCAL_SLOT" in
  */backend) printf backend-token ;;
  */target)
    if [ "$1" = viewer@example.test ]; then printf viewer-token; else printf target-token; fi
    ;;
  *) exit 23 ;;
esac
""")
        self.executable(self.source / "cli/target/release/cloudthinker", """#!/usr/bin/env python3
import json, os, sys
if sys.argv[1:] == ['--version']:
    print('cloudthinker 0.7.3-dev.2')
elif sys.argv[1:] == ['whoami', '--json']:
    print(json.dumps({'workspace_id': 'backend-workspace'}))
elif sys.argv[1:] != ['whoami']:
    print(json.dumps({'args': sys.argv[1:], 'cwd': os.getcwd(), 'env': {
        key: value for key, value in os.environ.items()
        if key.startswith(('CLOUDTHINKER_', 'CYBER_', 'CT_CLI_'))
    }}))
""")

    def executable(self, path, contents):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
        path.chmod(0o700)

    def launch(self, *args):
        return subprocess.run(
            ["bash", str(self.script), *args], cwd=self.target,
            env=self.env, text=True, capture_output=True, timeout=10,
        )

    def run_source_adapter(self, env_overrides, *args):
        source = self.root / "adapter source"
        (source / "agent-cli/node_modules").mkdir(parents=True)
        mock_bin = self.root / "adapter-bin"
        self.executable(mock_bin / "bun", """#!/usr/bin/env python3
import json, os, sys
print(json.dumps({
    "argv": sys.argv[1:],
    "cwd": os.getcwd(),
    "cloudthinker_bin": os.environ.get("CLOUDTHINKER_BIN"),
    "local_auth_bin": os.environ.get("CLOUDTHINKER_LOCAL_AUTH_BIN"),
}))
""")
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("CLOUDTHINKER_", "CYBER_", "CT_CLI_"))
        }
        env.update({
            "PATH": f"{mock_bin}:/usr/bin:/bin",
            "CT_CLI_SOURCE": str(source),
        })
        env.update(env_overrides)
        adapter = Path(__file__).resolve().parents[2] / "tools/ct-cli/scripts/agent-source.sh"
        return subprocess.run(
            ["bash", str(adapter), *args],
            cwd=self.target,
            env=env,
            text=True,
            capture_output=True,
            timeout=10,
        )

    def test_bare_launch_preserves_target_and_uses_separate_credentials(self):
        result = self.launch()
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["args"], [])
        self.assertEqual(payload["cwd"], str(self.target))
        self.assertEqual(payload["env"]["CLOUDTHINKER_URL"], "http://backend.test")
        self.assertEqual(payload["env"]["CLOUDTHINKER_TOKEN"], "backend-token")
        self.assertEqual(payload["env"]["CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY"], "4")
        self.assertEqual(
            payload["env"]["CLOUDTHINKER_LOCAL_AUTH_BIN"],
            str(self.source / "cli/scripts/cloudthinker-local"),
        )
        self.assertEqual(payload["env"]["CYBER_OWNER_AUTH"], "Authorization: Bearer target-token")
        self.assertEqual(payload["env"]["CT_CLI_SOURCE"], str(self.source))
        self.assertEqual(payload["env"]["CLOUDTHINKER_AGENT_BIN"], str(self.source / "tools/ct-cli/scripts/agent-source.sh"))
        self.assertIn("Test stack: target", result.stderr)

    def test_workflow_concurrency_override_is_forwarded(self):
        self.env["CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY"] = "8"
        result = self.launch("--", "agent", "-p", "hello")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout)["env"]["CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY"],
            "8",
        )

    def test_invalid_workflow_concurrency_fails_before_authentication(self):
        self.env["CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY"] = "9"
        result = self.launch()
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY must be an integer from 1 to 8", result.stderr)
        self.assertNotIn("backend-token", result.stderr)

    def test_does_not_reuse_backend_token_when_no_target_configured(self):
        del self.env["CLOUDTHINKER_TARGET_SLOT"]
        result = self.launch("--", "cyber", "doctor")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("CYBER_OWNER_AUTH", json.loads(result.stdout)["env"])

    def test_target_auth_failure_stops_launch(self):
        self.executable(self.helper, """#!/bin/sh
case "$CLOUDTHINKER_LOCAL_SLOT" in
  */backend) printf backend-token ;;
  *) echo 'target identity missing' >&2; exit 23 ;;
esac
""")
        result = self.launch()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("target identity missing", result.stderr)
        self.assertNotIn("backend-token", result.stderr)

    def test_version_does_not_need_running_stacks_or_tokens(self):
        self.helper.unlink()
        result = self.launch("--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "cloudthinker 0.7.3-dev.2\n")

    def test_arguments_pass_through_without_shell_interpretation(self):
        for prefix in ([], ["--"]):
            with self.subTest(prefix=prefix):
                args = ["agent", "-p", "two words; $(exit 9)"]
                result = self.launch(*prefix, *args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["args"], args)

    def test_agent_token_request_checks_workspace_before_using_env_token(self):
        self.env["CLOUDTHINKER_WORKSPACE"] = "backend-workspace"
        result = self.launch("auth", "token", "--workspace", "backend-workspace")
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["args"], ["auth", "token"])
        self.assertNotIn("CLOUDTHINKER_WORKSPACE", payload["env"])

    def test_agent_token_request_skips_build_and_target_setup(self):
        self.executable(self.root / "cargo/bin/cargo", """#!/bin/sh
case "$*" in
  *' build '*) exit 91 ;;
  *'env --json'*) printf '{\"backend_url\":\"http://backend.test\"}\\n' ;;
esac
""")
        self.executable(self.helper, """#!/bin/sh
case "$CLOUDTHINKER_LOCAL_SLOT" in
  */backend) printf backend-token ;;
  *) echo 'target setup should be skipped' >&2; exit 23 ;;
esac
""")
        result = self.launch("auth", "token")
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["args"], ["auth", "token"])
        self.assertEqual(payload["env"]["CLOUDTHINKER_TOKEN"], "backend-token")

    def test_local_auth_binary_override_is_forwarded(self):
        auth_bin = self.root / "fast-auth"
        self.executable(auth_bin, "#!/bin/sh\nexit 0\n")
        self.env["CLOUDTHINKER_LOCAL_AUTH_BIN"] = str(auth_bin)

        result = self.launch("auth", "token")

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["env"]["CLOUDTHINKER_LOCAL_AUTH_BIN"], str(auth_bin))

    def test_source_adapter_pins_local_auth_binary_for_bun(self):
        auth_bin = self.root / "native-wrapper"
        self.executable(auth_bin, "#!/bin/sh\nexit 0\n")

        result = self.run_source_adapter(
            {"CLOUDTHINKER_BIN": "/native/current", "CLOUDTHINKER_LOCAL_AUTH_BIN": str(auth_bin)},
            "--mode",
            "json",
            "-p",
            "hello",
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["cloudthinker_bin"], str(auth_bin))
        self.assertEqual(payload["local_auth_bin"], str(auth_bin))
        self.assertEqual(payload["cwd"], str(self.target))
        self.assertEqual(
            payload["argv"][1:],
            ["--mode", "json", "-p", "hello"],
        )

    def test_source_adapter_keeps_native_binary_without_local_auth_bin(self):
        result = self.run_source_adapter({"CLOUDTHINKER_BIN": "/native/current"}, "--version")

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["cloudthinker_bin"], "/native/current")
        self.assertIsNone(payload["local_auth_bin"])

    def test_source_adapter_rejects_invalid_local_auth_binary(self):
        result = self.run_source_adapter({"CLOUDTHINKER_LOCAL_AUTH_BIN": "relative-wrapper"})

        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("CLOUDTHINKER_LOCAL_AUTH_BIN", result.stderr)

    def test_agent_token_request_rejects_another_workspace(self):
        result = self.launch("auth", "token", "--workspace", "target-workspace")
        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stdout, "")
        self.assertIn("does not match", result.stderr)

    def test_global_workspace_selection_uses_same_verified_identity(self):
        for args in (
            ["--workspace", "backend-workspace", "auth", "token"],
            ["--workspace=backend-workspace", "auth", "token"],
        ):
            with self.subTest(args=args):
                result = self.launch("--", *args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["args"], ["auth", "token"])

    def test_target_owner_must_be_explicit(self):
        del self.env["CLOUDTHINKER_TARGET_EMAIL"]
        result = self.launch()
        self.assertEqual(result.returncode, 2)
        self.assertIn("target owner account", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_agent_passthrough_preserves_workspace_flag_after_delimiter(self):
        args = ["agent", "--", "--workspace", "agent-option"]
        result = self.launch("--", *args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["args"], args)

    def test_empty_workspace_does_not_silently_select_default(self):
        for args in (["--workspace", ""], ["--workspace="]):
            with self.subTest(args=args):
                result = self.launch("--", *args, "auth", "token")
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")

    def test_viewer_is_minted_separately_for_role_comparisons(self):
        self.env["CLOUDTHINKER_TARGET_VIEWER_EMAIL"] = "viewer@example.test"
        result = self.launch("cyber", "doctor")
        self.assertEqual(result.returncode, 0, result.stderr)
        env = json.loads(result.stdout)["env"]
        self.assertEqual(env["CYBER_VIEWER_AUTH"], "Authorization: Bearer viewer-token")
        self.assertEqual(env["CYBER_OWNER_AUTH"], "Authorization: Bearer target-token")

    def test_real_token_helper_uses_selected_slot_for_cli_and_compose(self):
        helper = self.source / ".agents/skills/cloudthinker-cli-kit/scripts/mint_token.sh"
        helper.parent.mkdir(parents=True, exist_ok=True)
        real_helper = Path(__file__).resolve().parents[2] / ".agents/skills/cloudthinker-cli-kit/scripts/mint_token.sh"
        shutil.copyfile(real_helper, helper)
        helper.chmod(0o700)

        selected = self.backend
        (selected / "tools/ct-cli").mkdir(parents=True, exist_ok=True)
        (selected / "tools/ct-cli/Cargo.toml").write_text("[workspace]\n")
        (self.source / ".env").write_text("COMPOSE_PROJECT_NAME=source-project\n")
        (self.source / ".env.local").write_text("")
        (selected / ".env").write_text("COMPOSE_PROJECT_NAME=selected-project\n")
        (selected / ".env.local").write_text("")
        mock_bin = self.root / "mock-bin"
        log_path = self.root / "commands.jsonl"
        self.executable(mock_bin / "cargo", """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["COMMAND_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps({"program": "cargo", "argv": sys.argv[1:], "cwd": os.getcwd()}) + "\\n")
sql = sys.argv[-1]
if 'select u.id, u.email from "user"' in sql:
    print(json.dumps([{"id": "fixture-user", "email": "fixture@example.test"}]))
elif "select id from workspace" in sql:
    print(json.dumps([{"id": "fixture-workspace"}]))
else:
    print("[]")
""")
        self.executable(mock_bin / "docker", """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["COMMAND_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps({"program": "docker", "argv": sys.argv[1:], "cwd": os.getcwd()}) + "\\n")
print("fixture-token")
""")

        for override in (False, True):
            with self.subTest(override=override):
                log_path.unlink(missing_ok=True)
                env = {
                    k: v
                    for k, v in os.environ.items()
                    if not k.startswith(("CLOUDTHINKER_", "CYBER_", "CT_CLI_"))
                }
                env.update({"PATH": f"{mock_bin}:{os.environ['PATH']}", "COMMAND_LOG": str(log_path)})
                if override:
                    env["CLOUDTHINKER_LOCAL_SLOT"] = str(selected)
                result = subprocess.run(
                    ["bash", str(helper)],
                    cwd=self.target,
                    env=env,
                    text=True,
                    capture_output=True,
                    timeout=10,
                )
                self.assertEqual(
                    result.returncode,
                    0,
                    f"{result.stderr}\n{result.stdout}\n{log_path.read_text() if log_path.exists() else ''}",
                )
                self.assertEqual(result.stdout, "fixture-token\n")
                commands = [json.loads(line) for line in log_path.read_text().splitlines()]
                expected_root = selected if override else self.source
                cargo_calls = [call for call in commands if call["program"] == "cargo"]
                docker_calls = [call for call in commands if call["program"] == "docker"]
                self.assertEqual(len(cargo_calls), 2)
                self.assertEqual(len(docker_calls), 1)
                for call in cargo_calls:
                    self.assertEqual(call["cwd"], str(expected_root))
                    manifest = call["argv"][call["argv"].index("--manifest-path") + 1]
                    self.assertEqual(manifest, str(expected_root / "tools/ct-cli/Cargo.toml"))
                self.assertEqual(docker_calls[0]["cwd"], str(expected_root))


if __name__ == "__main__":
    unittest.main()
