from pathlib import Path
import tempfile
import unittest

from changelog import check, fold, git


class ChangelogTests(unittest.TestCase):
    def test_ca_ad_10_fragment_required_and_released_content_is_immutable(self):
        with tempfile.TemporaryDirectory(prefix="ct-changelog-") as directory:
            root = Path(directory)
            git(root, "init", "-q")
            git(root, "config", "user.name", "Fixture")
            git(root, "config", "user.email", "fixture@example.invalid")
            source = root / "cli/crates/cloudthinker-cli/src/main.rs"
            source.parent.mkdir(parents=True)
            source.write_text("initial")
            (root / "cli/Cargo.toml").write_text("fixture")
            changelog = root / "cli/CHANGELOG.md"
            changelog.write_text("## [0.1.0]\n\n- Initial release.\n")
            git(root, "add", ".")
            git(root, "commit", "-qm", "initial")
            base = git(root, "rev-parse", "HEAD")
            source.write_text("changed")
            with self.assertRaises(ValueError):
                check(root, base)
            check(root, base, "no-changelog")
            fragment = root / "cli/.changes/change.md"
            fragment.parent.mkdir()
            fragment.write_text("")
            with self.assertRaises(ValueError):
                check(root, base)
            fragment.write_text("- Improved startup.\n")
            check(root, base)
            empty = fragment.parent / "empty.md"
            empty.write_text("")
            git(root, "add", ".")
            git(root, "commit", "-qm", "change")
            fold(root, "0.2.0")
            self.assertFalse(fragment.exists())
            self.assertTrue(empty.exists())
            self.assertEqual(changelog.read_text(), "## [0.2.0]\n\n- Improved startup.\n\n## [0.1.0]\n\n- Initial release.\n")
            check(root, base, "no-changelog")
            changelog.write_text("rewritten history")
            with self.assertRaises(ValueError):
                check(root, base, "no-changelog")

    def test_ca_ad_10_mirror_paths_nested_fragments_and_first_appearance_order(self):
        with tempfile.TemporaryDirectory(prefix="ct-changelog-") as directory:
            root = Path(directory)
            git(root, "init", "-q")
            git(root, "config", "user.name", "Fixture")
            git(root, "config", "user.email", "fixture@example.invalid")
            source = root / "crates/cloudthinker-cli/src/main.rs"
            source.parent.mkdir(parents=True)
            source.write_text("initial")
            git(root, "add", ".")
            git(root, "commit", "-qm", "initial")
            base = git(root, "rev-parse", "HEAD")
            source.write_text("changed")
            fragment = root / ".changes/first.md"
            fragment.parent.mkdir()
            fragment.write_text("- First change.\n")
            check(root, base)
            nested = fragment.parent / "nested/ignored.md"
            nested.parent.mkdir()
            nested.write_text("- Hidden change.\n")
            with self.assertRaises(ValueError):
                check(root, base)
            nested.unlink()
            git(root, "add", ".")
            git(root, "commit", "-qm", "first")
            fold(root, "0.1.0")
            self.assertEqual((root / "CHANGELOG.md").read_text(), "## [0.1.0]\n\n- First change.\n\n")


if __name__ == "__main__":
    unittest.main()
