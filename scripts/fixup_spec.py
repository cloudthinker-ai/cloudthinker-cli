#!/usr/bin/env python3
"""Patch the 3.0 gaps `@apiture/openapi-down-convert` leaves behind.

Run AFTER the 3.1→3.0 down-convert, on the pruned doc. Three fixes, each a
progenitor blocker if left (arch plan §Codegen spike):

1. `anyOf`/`oneOf` carrying a `{"type": "null"}` member — Pydantic v2's nullable
   idiom. Collapse to the single non-null subschema + `nullable: true`; a bare
   two-member `[X, null]` becomes X with nullable set. Left alone, progenitor
   emits an untagged enum for a value that is really "X or absent".
2. Numeric `exclusiveMinimum`/`exclusiveMaximum` (3.1 number form) → 3.0 boolean
   form (`minimum` + `exclusiveMinimum: true`). The down-converter misses some.
3. Duplicate `operationId`s → de-duplicate by suffixing. The live app dedupes
   via `custom_generate_unique_id`, so this is a no-op safety net (asserts none
   were actually renamed unless the spec regresses).
"""

from __future__ import annotations

import json
import sys


def _fix_nullable(node: object) -> object:
    """Collapse Pydantic `anyOf/oneOf … {type: null}` into `nullable: true`."""
    if isinstance(node, list):
        return [_fix_nullable(item) for item in node]
    if not isinstance(node, dict):
        return node

    node = {key: _fix_nullable(value) for key, value in node.items()}

    for combinator in ("anyOf", "oneOf"):
        members = node.get(combinator)
        if not isinstance(members, list):
            continue
        non_null = [m for m in members if not (isinstance(m, dict) and m.get("type") == "null")]
        has_null = len(non_null) != len(members)
        if not has_null:
            continue
        if len(non_null) == 1:
            # Single real subschema: merge it up and mark nullable. Sibling
            # keys (title, description, default) on `node` win over the member.
            merged = dict(non_null[0])
            merged["nullable"] = True
            for key, value in node.items():
                if key != combinator:
                    merged.setdefault(key, value)
            return merged
        # Multiple real subschemas stay a union; just drop the null member and
        # flag nullable at the union level.
        node[combinator] = non_null
        node["nullable"] = True
    return node


def _fix_exclusive_bounds(node: object) -> None:
    """Rewrite 3.1 numeric exclusive bounds to the 3.0 boolean form in place."""
    if isinstance(node, dict):
        for bound in ("exclusiveMinimum", "exclusiveMaximum"):
            value = node.get(bound)
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                base = "minimum" if bound == "exclusiveMinimum" else "maximum"
                node[base] = value
                node[bound] = True
        for value in node.values():
            _fix_exclusive_bounds(value)
    elif isinstance(node, list):
        for item in node:
            _fix_exclusive_bounds(item)


def _dedupe_operation_ids(spec: dict) -> None:
    seen: set[str] = set()
    renamed: list[str] = []
    for path_item in spec.get("paths", {}).values():
        if not isinstance(path_item, dict):
            continue
        for op in path_item.values():
            if not isinstance(op, dict) or "operationId" not in op:
                continue
            op_id = op["operationId"]
            if op_id in seen:
                suffix = 2
                while f"{op_id}_{suffix}" in seen:
                    suffix += 1
                op_id = f"{op_id}_{suffix}"
                op["operationId"] = op_id
                renamed.append(op_id)
            seen.add(op_id)
    if renamed:
        # The live app's custom_generate_unique_id already guarantees uniqueness;
        # a rename here means the spec regressed and codegen fn names shifted.
        print(f"fixup_spec: WARNING deduped operationIds: {renamed}", file=sys.stderr)


def fixup(spec: dict) -> dict:
    spec = _fix_nullable(spec)  # returns a rebuilt tree
    _fix_exclusive_bounds(spec)
    _dedupe_operation_ids(spec)
    return spec


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: fixup_spec.py <input.json> <output.json>")
    with open(sys.argv[1]) as fh:
        spec = json.load(fh)
    spec = fixup(spec)
    with open(sys.argv[2], "w") as fh:
        json.dump(spec, fh, indent=2, sort_keys=True)
        fh.write("\n")
    print("fixup_spec: applied nullable + exclusive-bound fixes")


if __name__ == "__main__":
    main()
