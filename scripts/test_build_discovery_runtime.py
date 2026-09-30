from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path


CLI_ROOT = Path(__file__).resolve().parents[1]
BUILDER = CLI_ROOT / "scripts/build_discovery_runtime.py"
RUNTIME = CLI_ROOT / "crates/cloudthinker-cli/runtime/cyber-discovery.pyz"


class DiscoveryRuntimeResourcesTest(unittest.TestCase):
    def test_resources_match_canonical_sources(self) -> None:
        result = subprocess.run(
            [sys.executable, str(BUILDER), "--check"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_contains_only_pure_python_yaml(self) -> None:
        with zipfile.ZipFile(RUNTIME) as archive:
            names = archive.namelist()
            self.assertIn("__main__.py", names)
            self.assertIn("appsec_discovery/runner.py", names)
            self.assertIn("appsec_curl.py", names)
            self.assertIn("appsec_auth.py", names)
            self.assertIn("vendor/PyYAML-LICENSE", names)
            self.assertTrue(
                any(name.startswith("yaml/") and name.endswith(".py") for name in names)
            )
            self.assertFalse(
                any(name.endswith((".so", ".pyd", ".dll")) for name in names)
            )
            metadata = json.loads(archive.read("vendor/runtime.json"))
            self.assertEqual(metadata["pyyaml_version"], "6.0.3")
            self.assertEqual(
                metadata["pyyaml_sdist_sha256"],
                "d76623373421df22fb4cf8817020cbb7ef15c725b9d5e45f17e189bfc384190f",
            )
            for name, digest in metadata["sources"].items():
                self.assertEqual(hashlib.sha256(archive.read(name)).hexdigest(), digest)
            for name, digest in metadata["members"].items():
                self.assertEqual(hashlib.sha256(archive.read(name)).hexdigest(), digest)


    def test_imports_in_isolated_python(self) -> None:
        result = subprocess.run(
            [sys.executable, "-I", str(RUNTIME), "--help"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


    def test_loads_canonical_runner_without_target_traffic(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            manifest = root / "manifest.json"
            manifest.write_text("{}")
            result = subprocess.run(
                [
                    sys.executable,
                    "-I",
                    str(RUNTIME),
                    "run",
                    "--manifest",
                    str(manifest),
                    "--discovery-dir",
                    str(root / "discovery"),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(result.returncode, 1)
        self.assertIn("invalid discovery manifest version", result.stderr)

    def test_validate_entrypoint_uses_canonical_manifest_loader(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            manifest = root / "manifest.json"
            report = root / "report.full.json"
            manifest.write_text("{}")
            report.write_text("{}")
            result = subprocess.run(
                [
                    sys.executable,
                    "-I",
                    str(RUNTIME),
                    "validate",
                    "--manifest",
                    str(manifest),
                    "--report",
                    str(report),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(result.returncode, 1)
        self.assertIn("invalid discovery manifest version", result.stderr)


if __name__ == "__main__":
    unittest.main()
