import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest


CLI = Path(__file__).resolve().parents[1]


class CheckReleaseTests(unittest.TestCase):
    def test_agent_sidecars_are_required_and_public(self):
        config = tomllib.loads((CLI / "dist-workspace.toml").read_text())
        agent_assets = [
            Path(asset).name
            for group in config["dist"]["extra-artifacts"]
            for asset in group["artifacts"]
        ]
        assets = agent_assets + [
            f"cloudthinker-cli-{target}.{extension}"
            for target, extension in [
                ("aarch64-apple-darwin", "tar.xz"),
                ("x86_64-apple-darwin", "tar.xz"),
                ("aarch64-unknown-linux-gnu", "tar.xz"),
                ("x86_64-unknown-linux-gnu", "tar.xz"),
                ("x86_64-pc-windows-msvc", "zip"),
            ]
        ] + ["cloudthinker-cli-installer.sh", "cloudthinker-cli-installer.ps1", "sha256.sum"]
        sidecars = [name for name in agent_assets if name.endswith((".sha256", "-sha256.sum"))]
        cases = [(None, None, 0)] + [
            (name if missing else None, None if missing else name, 1)
            for name in sidecars
            for missing in (True, False)
        ]
        with tempfile.TemporaryDirectory(prefix="check-release-test-") as directory:
            curl = Path(directory) / "curl"
            curl.write_text(
                "#!/usr/bin/env python3\n"
                "import os, sys\n"
                "if sys.argv[-1].startswith('https://api.github.com/'):\n"
                "    print(os.environ['TEST_RELEASE'])\n"
                "elif sys.argv[-1].endswith('/' + os.environ.get('TEST_INACCESSIBLE', '')):\n"
                "    sys.exit(22)\n"
            )
            curl.chmod(0o755)
            for omitted, inaccessible, expected in cases:
                with self.subTest(omitted=omitted, inaccessible=inaccessible):
                    result = subprocess.run(
                        ["bash", str(CLI / "scripts/check-release.sh")],
                        env={
                            **os.environ,
                            "PATH": directory + os.pathsep + os.environ["PATH"],
                            "TAG": "v-test",
                            "TEST_RELEASE": json.dumps({
                                "tag_name": "v-test",
                                "assets": [{"name": name} for name in assets if name != omitted],
                            }),
                            "TEST_INACCESSIBLE": inaccessible or "",
                        },
                        capture_output=True,
                        text=True,
                        timeout=15,
                    )
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                    if omitted or inaccessible:
                        self.assertIn(omitted or inaccessible, result.stderr)


if __name__ == "__main__":
    unittest.main()
