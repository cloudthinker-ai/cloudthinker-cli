#!/usr/bin/env python3
"""Prune the full backend OpenAPI doc to the CLI path allowlist + its schema closure.

The full spec (≈700 paths / 1200 schemas) makes progenitor emit duplicate
definitions and type cycles that fail `cargo check` (arch plan §Codegen spike).
The CLI only needs five endpoints, so we keep exactly those paths plus the
transitive `$ref` closure of their request/response schemas and drop everything
else — the allowlist doubles as the CLI's explicit contract surface.

Auth is injected client-side (a Bearer header set by `cloudthinker-client`), so
per-operation `security` and `components.securitySchemes` are stripped: the
generated operations take no auth params.

Input is OpenAPI 3.1 (fresh dump); output stays 3.1 for the down-convert step.
"""

from __future__ import annotations

import json
import sys

# The endpoints the CLI drives. Keep this list in lockstep with the CLI
# commands — an endpoint absent here never reaches the generated client.
ALLOWED_PATHS = frozenset(
    {
        "/api/v1/login/cli/token",
        "/api/v1/login/refresh",
        "/api/v1/login/logout",
        "/api/v1/cli/runs",
        "/api/v1/cli/runs/{run_id}",
        # `review` command: coordinate -> review-detail lookup (MR-F).
        "/api/v1/code-review/merge-requests/lookup",
    }
)

_HTTP_METHODS = frozenset({"get", "put", "post", "delete", "patch", "options", "head"})


def _collect_refs(node: object, out: set[str]) -> None:
    """Add the schema name of every `#/components/schemas/*` $ref under ``node``."""
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "$ref" and isinstance(value, str):
                out.add(value.rsplit("/", 1)[-1])
            else:
                _collect_refs(value, out)
    elif isinstance(node, list):
        for item in node:
            _collect_refs(item, out)


def _transitive_schemas(seeds: set[str], schemas: dict[str, object]) -> set[str]:
    """Return ``seeds`` plus every schema reachable from them via $ref."""
    seen: set[str] = set()
    stack = list(seeds)
    while stack:
        name = stack.pop()
        if name in seen:
            continue
        seen.add(name)
        schema = schemas.get(name)
        if schema is None:
            raise SystemExit(f"prune_spec: dangling $ref to missing schema '{name}'")
        found: set[str] = set()
        _collect_refs(schema, found)
        stack.extend(found - seen)
    return seen


def prune(spec: dict) -> dict:
    missing = ALLOWED_PATHS - set(spec.get("paths", {}))
    if missing:
        raise SystemExit(f"prune_spec: allowlisted paths absent from spec: {sorted(missing)}")

    kept_paths: dict[str, dict] = {}
    seeds: set[str] = set()
    for path in ALLOWED_PATHS:
        item = dict(spec["paths"][path])
        for method in list(item):
            if method.lower() in _HTTP_METHODS:
                op = dict(item[method])
                # Auth is a client-injected Bearer header, not a codegen concern.
                op.pop("security", None)
                item[method] = op
        _collect_refs(item, seeds)
        kept_paths[path] = item

    all_schemas = spec.get("components", {}).get("schemas", {})
    kept_names = _transitive_schemas(seeds, all_schemas)
    kept_schemas = {name: all_schemas[name] for name in sorted(kept_names)}

    pruned = dict(spec)
    pruned["paths"] = {p: kept_paths[p] for p in sorted(kept_paths)}
    components = dict(spec.get("components", {}))
    components["schemas"] = kept_schemas
    # Client injects auth; drop the scheme so operations take no auth params.
    components.pop("securitySchemes", None)
    pruned["components"] = components
    pruned.pop("security", None)
    return pruned


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: prune_spec.py <input.json> <output.json>")
    with open(sys.argv[1]) as fh:
        spec = json.load(fh)
    pruned = prune(spec)
    with open(sys.argv[2], "w") as fh:
        json.dump(pruned, fh, indent=2, sort_keys=True)
    n_paths = len(pruned["paths"])
    n_schemas = len(pruned["components"]["schemas"])
    print(f"prune_spec: kept {n_paths} paths, {n_schemas} schemas")


if __name__ == "__main__":
    main()
