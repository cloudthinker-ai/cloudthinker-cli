from __future__ import annotations

import argparse
import contextlib
import json
import subprocess
import sys
from pathlib import Path

from appsec_discovery.reporting import _load_report
from appsec_discovery.runner import run_discovery
from discovery_limits import (
    DEFAULT_DISCOVERY_TIMEOUT_SECONDS,
    DEFAULT_NORMALIZED_CANDIDATES,
)
from discovery_schema import VERSION, validate_report


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="cyber-discovery")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("version")
    run = sub.add_parser("run")
    run.add_argument("--manifest", required=True, type=Path)
    run.add_argument("--discovery-dir", required=True, type=Path)
    run.add_argument(
        "--max-urls",
        default=DEFAULT_NORMALIZED_CANDIDATES,
        type=int,
    )
    run.add_argument(
        "--timeout",
        default=DEFAULT_DISCOVERY_TIMEOUT_SECONDS,
        type=int,
    )
    run.add_argument("--seed", action="append", default=[])
    run.add_argument("--depth", type=int)
    run.add_argument("--insecure", action="store_true")
    validate = sub.add_parser("validate")
    validate.add_argument("--manifest", required=True, type=Path)
    validate.add_argument("--report", required=True, type=Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command == "version":
        print(VERSION)
        return 0
    if args.command == "validate":
        try:
            report = _load_report(args.report, manifest_path=args.manifest)
        except (ValueError, OSError, TypeError, KeyError) as exc:
            print(f"cyber-discovery: {exc}", file=sys.stderr)
            return 1
        print(
            json.dumps(
                {
                    "valid": True,
                    "overall_state": report["overall_state"],
                    "total_candidates": report["total_candidates"],
                },
                sort_keys=True,
            )
        )
        return 0
    try:
        with contextlib.redirect_stdout(sys.stderr):
            report = run_discovery(
                manifest_path=args.manifest,
                discovery_dir=args.discovery_dir,
                max_urls=args.max_urls,
                deadline_seconds=args.timeout,
                persist_surface=False,
                seeds=args.seed,
                depth=args.depth,
                insecure=args.insecure,
            )
        validate_report(report)
        report = _load_report(
            args.discovery_dir / "report.full.json",
            manifest_path=args.manifest,
        )
    except (
        ValueError,
        OSError,
        subprocess.SubprocessError,
        json.JSONDecodeError,
        TypeError,
    ) as exc:
        print(f"cyber-discovery: {exc}", file=sys.stderr)
        return 1
    print(
        json.dumps(
            {
                "overall_state": report["overall_state"],
                "total_candidates": report["total_candidates"],
            },
            sort_keys=True,
        )
    )
    return 1 if report["overall_state"] == "FAILED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
