from __future__ import annotations

import argparse
import ast
import hashlib
import io
import json
import re
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
CLI_ROOT = REPO_ROOT / "cli"
EXECUTOR_SCRIPTS = (
    REPO_ROOT
    / "executor/app/skills/public/appsec/scan/scripts"
)
RUNTIME_SOURCE = CLI_ROOT / "crates/cloudthinker-cli/runtime/__main__.py"
RUNTIME_ASSET = CLI_ROOT / "crates/cloudthinker-cli/runtime/cyber-discovery.pyz"
BACKEND_PACKAGE = REPO_ROOT / "backend/app/features/appsec/domain/discovery_contract"
SCHEMA_SOURCE = EXECUTOR_SCRIPTS / "discovery_schema.py"
LIMITS_SOURCE = EXECUTOR_SCRIPTS / "discovery_limits.py"
LOCKFILE = REPO_ROOT / "executor/uv.lock"
COMMON_MODULES = (
    "appsec_auth.py",
    "appsec_curl.py",
    "appsec_routes.py",
    "appsec_scope.py",
    "appsec_source_routes.py",
    "discovery_limits.py",
    "discovery_schema.py",
    "http_methods.py",
)
PYTHON_VERSION = (3, 10)
ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)
BACKEND_INIT = (
    "from .limits import MODEL_DIGEST_TOKEN_LIMIT\n"
    "from .schema import (\n"
    "    COLLECTOR_COUNT_FIELDS,\n"
    "    COLLECTOR_RESULT_KEYS,\n"
    "    COLLECTOR_STATES,\n"
    "    COLLECTORS,\n"
    "    COUNT_FIELDS,\n"
    "    OVERALL_STATES,\n"
    "    REPORT_KEYS,\n"
    "    VERSION,\n"
    "    DiscoveryReportValidationError,\n"
    "    validate_report,\n"
    "    validate_report_gate,\n"
    ")\n"
    "\n"
    "__all__ = [\n"
    "    \"COLLECTORS\",\n"
    "    \"COLLECTOR_COUNT_FIELDS\",\n"
    "    \"COLLECTOR_RESULT_KEYS\",\n"
    "    \"COLLECTOR_STATES\",\n"
    "    \"COUNT_FIELDS\",\n"
    "    \"MODEL_DIGEST_TOKEN_LIMIT\",\n"
    "    \"OVERALL_STATES\",\n"
    "    \"REPORT_KEYS\",\n"
    "    \"VERSION\",\n"
    "    \"DiscoveryReportValidationError\",\n"
    "    \"validate_report\",\n"
    "    \"validate_report_gate\",\n"
    "]\n"
)
SCHEMA_IMPORT = "from discovery_limits import MODEL_DIGEST_TOKEN_LIMIT"
SCHEMA_GENERATED_IMPORT = "from .limits import MODEL_DIGEST_TOKEN_LIMIT"


def _load_pyyaml_lock() -> tuple[str, str, str, int]:
    lock = LOCKFILE.read_text()
    match = re.search(
        r'(?ms)^name = "pyyaml"\nversion = "([^"]+)".*?'
        r'^sdist = \{ url = "([^"]+)", hash = "sha256:([0-9a-f]+)", size = (\d+)',
        lock,
    )
    if match is None:
        raise ValueError("PyYAML source archive is missing from executor/uv.lock")
    version, url, digest, size = match.groups()
    return version, url, digest, int(size)


def _pyyaml_files_from_archive(data: bytes, expected_digest: str) -> dict[str, bytes]:
    actual_digest = hashlib.sha256(data).hexdigest()
    if actual_digest != expected_digest:
        raise ValueError("PyYAML source archive checksum does not match executor/uv.lock")
    files: dict[str, bytes] = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive.getmembers():
            if not member.isfile():
                continue
            path = Path(member.name)
            parts = path.parts
            if "yaml" in parts and parts[-1].endswith(".py"):
                start = parts.index("yaml")
                member_name = "/".join(parts[start:])
            elif parts[-1].casefold() in {"license", "license.txt", "license.md"}:
                member_name = "vendor/PyYAML-LICENSE"
            else:
                continue
            source = archive.extractfile(member)
            if source is None:
                raise ValueError(f"cannot read PyYAML archive member {member.name}")
            files[member_name] = source.read()
    if "yaml/__init__.py" not in files or "vendor/PyYAML-LICENSE" not in files:
        raise ValueError("PyYAML archive has no pure-Python package or license")
    return files


def _pyyaml_files_from_asset() -> tuple[dict[str, bytes], dict[str, str]] | None:
    if not RUNTIME_ASSET.is_file():
        return None
    with zipfile.ZipFile(RUNTIME_ASSET) as archive:
        try:
            metadata = json.loads(archive.read("vendor/runtime.json"))
        except (KeyError, json.JSONDecodeError):
            return None
        files = {
            name: archive.read(name)
            for name in archive.namelist()
            if name.startswith("yaml/") and name.endswith(".py")
        }
        license_name = "vendor/PyYAML-LICENSE"
        if license_name in archive.namelist():
            files[license_name] = archive.read(license_name)
        if not files or license_name not in files:
            return None
        source_hashes = metadata.get("sources", {})
        if not isinstance(source_hashes, dict) or any(
            hashlib.sha256(data).hexdigest() != source_hashes.get(name)
            for name, data in files.items()
            if name.endswith(".py")
        ):
            return None
        member_hashes = metadata.get("members", {})
        if not isinstance(member_hashes, dict) or any(
            hashlib.sha256(data).hexdigest() != member_hashes.get(name)
            for name, data in files.items()
        ):
            return None
        return files, metadata


def _pyyaml_files(*, refresh: bool) -> tuple[dict[str, bytes], dict[str, str]]:
    version, url, digest, size = _load_pyyaml_lock()
    if not refresh:
        cached = _pyyaml_files_from_asset()
        if cached is not None:
            files, metadata = cached
            if (
                metadata.get("pyyaml_version") == version
                and metadata.get("pyyaml_sdist_sha256") == digest
            ):
                return files, metadata
    with urllib.request.urlopen(url, timeout=30) as response:
        data = response.read(size + 1)
    if len(data) != size:
        raise ValueError("PyYAML source archive size does not match executor/uv.lock")
    files = _pyyaml_files_from_archive(data, digest)
    return files, {"pyyaml_version": version, "pyyaml_sdist_sha256": digest}


def _runtime_sources() -> dict[str, bytes]:
    sources: dict[str, bytes] = {
        "__main__.py": RUNTIME_SOURCE.read_bytes(),
    }
    package_root = EXECUTOR_SCRIPTS / "appsec_discovery"
    for source in sorted(package_root.rglob("*.py")):
        if "__pycache__" in source.parts:
            continue
        relative = source.relative_to(EXECUTOR_SCRIPTS).as_posix()
        sources[relative] = source.read_bytes()
    for name in COMMON_MODULES:
        path = EXECUTOR_SCRIPTS / name
        sources[name] = path.read_bytes()
    sources["ledger_lib/__init__.py"] = b""
    sources["ledger_lib/shared.py"] = (
        EXECUTOR_SCRIPTS / "ledger_lib/shared.py"
    ).read_bytes()
    return sources


def _local_imports(source: bytes, module_name: str) -> set[str]:
    tree = ast.parse(source, filename=module_name)
    found: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            found.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            package = module_name.rpartition(".")[0]
            if node.level:
                base_parts = package.split(".")
                keep = max(0, len(base_parts) - node.level + 1)
                relative_base = ".".join(base_parts[:keep])
                imported_module = ".".join(
                    part for part in (relative_base, node.module or "") if part
                )
            else:
                imported_module = node.module or ""
            if imported_module:
                found.add(imported_module)
    return found


def _check_runtime_closure(sources: dict[str, bytes]) -> None:
    modules: set[str] = set()
    for filename in sources:
        if not filename.endswith(".py"):
            continue
        path = Path(filename)
        parts = list(path.with_suffix("").parts)
        if parts[-1] == "__init__":
            parts.pop()
        modules.add(".".join(parts))
    missing: set[tuple[str, str]] = set()
    external = set(sys.stdlib_module_names) | {"yaml"}
    for filename, source in sources.items():
        if not filename.endswith(".py"):
            continue
        path = Path(filename).with_suffix("")
        parts = list(path.parts)
        if parts[-1] == "__init__":
            parts.pop()
        importing_module = ".".join(parts)
        for imported in _local_imports(source, importing_module):
            if imported.split(".", 1)[0] in external:
                continue
            if imported not in modules and not any(
                module.startswith(f"{imported}.") for module in modules
            ):
                missing.add((filename, imported))
    if missing:
        rows = ", ".join(f"{source}: {module}" for source, module in sorted(missing))
        raise ValueError(f"discovery runtime has missing imports: {rows}")


def _zip_bytes(files: dict[str, bytes]) -> bytes:
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_STORED) as archive:
        for name, data in sorted(files.items()):
            info = zipfile.ZipInfo(name, date_time=ZIP_TIMESTAMP)
            info.compress_type = zipfile.ZIP_STORED
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            archive.writestr(info, data)
    return output.getvalue()


def _package_bytes(*, refresh_pyyaml: bool) -> bytes:
    files = _runtime_sources()
    _check_runtime_closure(files)
    pyyaml, metadata = _pyyaml_files(refresh=refresh_pyyaml)
    files.update(pyyaml)
    manifest = {
        **metadata,
        "runtime_schema_version": _schema_version(),
        "sources": {
            name: hashlib.sha256(data).hexdigest()
            for name, data in sorted(files.items())
            if name.endswith(".py")
        },
        "members": {
            name: hashlib.sha256(data).hexdigest()
            for name, data in sorted(files.items())
        },
    }
    files["vendor/runtime.json"] = json.dumps(
        manifest, sort_keys=True, separators=(",", ":")
    ).encode() + b"\n"
    return _zip_bytes(files)


def _schema_version() -> int:
    tree = ast.parse(SCHEMA_SOURCE.read_text(), filename=str(SCHEMA_SOURCE))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "VERSION"
            for target in node.targets
        ):
            return ast.literal_eval(node.value)
    raise ValueError("discovery schema VERSION is missing")


def _standalone_asset_valid() -> bool:
    if not RUNTIME_ASSET.is_file():
        return False
    version, _, digest, _ = _load_pyyaml_lock() if LOCKFILE.is_file() else (
        "6.0.3",
        "",
        "d76623373421df22fb4cf8817020cbb7ef15c725b9d5e45f17e189bfc384190f",
        0,
    )
    with zipfile.ZipFile(RUNTIME_ASSET) as archive:
        names = set(archive.namelist())
        if not {"__main__.py", "vendor/runtime.json", "vendor/PyYAML-LICENSE"} <= names:
            return False
        metadata = json.loads(archive.read("vendor/runtime.json"))
        if metadata.get("pyyaml_version") != version:
            return False
        if metadata.get("pyyaml_sdist_sha256") != digest:
            return False
        source_hashes = metadata.get("sources")
        if not isinstance(source_hashes, dict):
            return False
        for member, expected in source_hashes.items():
            if member not in names or not member.endswith(".py"):
                return False
            if hashlib.sha256(archive.read(member)).hexdigest() != expected:
                return False
        member_hashes = metadata.get("members")
        if not isinstance(member_hashes, dict):
            return False
        for member, expected in member_hashes.items():
            if member not in names:
                return False
            if hashlib.sha256(archive.read(member)).hexdigest() != expected:
                return False
        if not any(name.startswith("yaml/") and name.endswith(".py") for name in names):
            return False
        if any(name.endswith((".so", ".pyd", ".dll")) for name in names):
            return False
    return True


def _backend_files() -> dict[Path, bytes]:
    schema = SCHEMA_SOURCE.read_text()
    if schema.count(SCHEMA_IMPORT) != 1:
        raise ValueError("discovery schema import shape changed; update generator")
    schema = schema.replace(
        "def validate_report(report: object) -> None:",
        "def validate_report(report: object) -> None:  "
        "# noqa: PLR0912, PLR0915 — canonical implementation remains in executor.",
        1,
    )
    return {
        BACKEND_PACKAGE / "__init__.py": BACKEND_INIT.encode(),
        BACKEND_PACKAGE / "limits.py": LIMITS_SOURCE.read_bytes(),
        BACKEND_PACKAGE / "schema.py": schema.replace(
            SCHEMA_IMPORT, SCHEMA_GENERATED_IMPORT, 1
        ).encode(),
    }


def _write_or_check(outputs: dict[Path, bytes], *, check: bool) -> int:
    mismatches: list[str] = []
    for path, expected in outputs.items():
        if check:
            if not path.is_file() or path.read_bytes() != expected:
                mismatches.append(path.relative_to(REPO_ROOT).as_posix())
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(expected)
    if mismatches:
        print(
            "generated discovery resources drifted: " + ", ".join(mismatches),
            file=sys.stderr,
        )
        return 1
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--refresh-pyyaml", action="store_true")
    args = parser.parse_args(argv)
    try:
        if not EXECUTOR_SCRIPTS.is_dir():
            if args.check and _standalone_asset_valid():
                return 0
            raise ValueError(
                "canonical executor sources are required to build the discovery runtime"
            )
        archive = _package_bytes(refresh_pyyaml=args.refresh_pyyaml)
        outputs = {
            RUNTIME_ASSET: archive,
            **_backend_files(),
        }
        return _write_or_check(outputs, check=args.check)
    except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile) as exc:
        print(f"discovery runtime generation failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
