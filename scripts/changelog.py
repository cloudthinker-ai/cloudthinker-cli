import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def packages(root):
    cli = "cli" if (root / "cli/Cargo.toml").is_file() else "."
    return {
        cli: [f"{cli}/crates/cloudthinker-cli/src/", f"{cli}/crates/cloudthinker-client/src/"],
        "agent-cli/packages/agent": ["agent-cli/packages/agent/src/"],
        "agent-cli/packages/pi": ["agent-cli/packages/pi/src/"],
    }


def bullets(path):
    lines = [line.strip() for line in path.read_text().splitlines() if line.strip()]
    if any(not line.startswith("- ") or not line[2:].strip() for line in lines):
        raise ValueError(f"{path}: fragments contain nonempty '- ' bullets only")
    return lines


def check(root, base, labels=""):
    changes = git(root, "diff", "--name-only", "--no-renames", base).splitlines()
    changes += git(root, "ls-files", "--others", "--exclude-standard").splitlines()
    tracked_before = set(git(root, "ls-tree", "-r", "--name-only", base).splitlines())
    errors = []
    for package, prefixes in packages(root).items():
        changelog = f"{package}/CHANGELOG.md".removeprefix("./")
        if changelog in tracked_before:
            old = git(root, "show", f"{base}:{changelog}")
            current = root / changelog
            if not current.is_file() or not current.read_text().rstrip().endswith(old):
                errors.append(f"{changelog}: published content changed")
        fragment_prefix = f"{package}/.changes/".removeprefix("./")
        fragments = [path for path in changes if path.startswith(fragment_prefix)]
        valid_new = False
        for fragment in fragments:
            if not re.fullmatch(re.escape(fragment_prefix) + r"[^/]+\.md", fragment):
                errors.append(f"{fragment}: fragments must be flat Markdown files")
                continue
            path = root / fragment
            if not path.is_file():
                if fragment in tracked_before:
                    old_fragment = git(root, "show", f"{base}:{fragment}")
                    released = (root / changelog).read_text() if (root / changelog).exists() else ""
                    if any(line.strip() and line.strip() not in released for line in old_fragment.splitlines()):
                        errors.append(f"{fragment}: removed without folding its notes into {changelog}")
                continue
            try:
                lines = bullets(path)
                valid_new |= bool(lines) and fragment not in tracked_before
            except ValueError as error:
                errors.append(str(error))
        if "no-changelog" not in labels.split(",") and any(
            path.startswith(tuple(prefix.removeprefix("./") for prefix in prefixes)) for path in changes
        ) and not valid_new:
            errors.append(f"{package}: source changed without a new nonempty changelog fragment")
    if errors:
        raise ValueError("\n".join(errors))


def fold(root, version):
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?", version):
        raise ValueError("version must be a semver release number")
    updates = []
    for package in packages(root):
        fragments = list((root / package / ".changes").glob("*.md"))
        entries = []
        consumed = []
        for fragment in fragments:
            lines = bullets(fragment)
            if not lines:
                print(f"warning: keeping empty fragment {fragment}", file=sys.stderr)
                continue
            timestamp = git(root, "log", "--diff-filter=A", "--format=%ct", "-1", "--", str(fragment.relative_to(root)))
            if not timestamp:
                raise ValueError(f"commit {fragment} before folding release notes")
            entries.append((int(timestamp), fragment.name, lines))
            consumed.append(fragment)
        if not entries:
            continue
        changelog = root / package / "CHANGELOG.md"
        previous = changelog.read_text() if changelog.exists() else ""
        if f"## [{version}]" in previous:
            raise ValueError(f"{changelog}: release {version} already exists")
        lines = [line for _, _, group in sorted(entries) for line in group]
        updates.append((changelog, f"## [{version}]\n\n" + "\n".join(lines) + "\n\n" + previous, consumed))
    for changelog, content, consumed in updates:
        changelog.write_text(content)
        for fragment in consumed:
            fragment.unlink()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["check", "fold", "pending"])
    parser.add_argument("--base", default=os.environ.get("CI_MERGE_REQUEST_DIFF_BASE_SHA", "origin/develop"))
    parser.add_argument("--version")
    args = parser.parse_args()
    root = Path(git(Path(__file__).parent, "rev-parse", "--show-toplevel"))
    if args.action == "check":
        check(root, args.base, os.environ.get("CI_MERGE_REQUEST_LABELS", ""))
    elif args.action == "fold":
        if not args.version:
            parser.error("fold requires --version")
        fold(root, args.version)
    else:
        pending = [str(path) for package in packages(root) for path in (root / package / ".changes").glob("*.md") if bullets(path)]
        if pending:
            raise ValueError("Fold and commit release fragments before release-sync:\n" + "\n".join(pending))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
