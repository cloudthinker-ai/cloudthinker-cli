#!/usr/bin/env python3
"""Patch the 3.0 gaps `@apiture/openapi-down-convert` leaves behind.

Run AFTER the 3.1→3.0 down-convert, on the pruned doc. Three progenitor
blockers (arch plan §Codegen spike) plus one determinism fix:

1. `anyOf`/`oneOf` carrying a `{"type": "null"}` member — Pydantic v2's nullable
   idiom. Collapse to the single non-null subschema + `nullable: true`; a bare
   two-member `[X, null]` becomes X with nullable set. Left alone, progenitor
   emits an untagged enum for a value that is really "X or absent".
2. Numeric `exclusiveMinimum`/`exclusiveMaximum` (3.1 number form) → 3.0 boolean
   form (`minimum` + `exclusiveMinimum: true`). The down-converter misses some.
3. Duplicate `operationId`s → de-duplicate by suffixing. The live app dedupes
   via `custom_generate_unique_id`, so this is a no-op safety net (asserts none
   were actually renamed unless the spec regresses).
4. `info.title` → pinned constant. FastAPI takes it from `PROJECT_NAME`, which
   differs per environment (CI exports `cloudthinker-ci`), so an unpinned title
   makes the snapshot a function of the dumping env and `validate:cli-spec-drift`
   reports drift on every run. The CLI's spec identity is fixed, not deploy-local.
5. Remove the device-token endpoint's generic 422 response. Progenitor 0.14
   supports one typed error body per operation; retaining the meaningful 400
   device-poll response gives the client typed RFC 8628 errors, while malformed
   local input is rejected before the request is sent.
6. Remove a 422 response when its JSON schema is identical to the default error
   response. Keeping both makes progenitor count two error variants and abort.
7. Remove generic ApiErrorResponse defaults and ranges. Progenitor discards the
   HTTP status when a typed error body fails to decode, so undeclared generic
   errors must stay UnexpectedResponse during detail-only response migration.
"""

from __future__ import annotations

import json
import sys

from prune_spec import prune_unused_schemas

SPEC_TITLE = "Cloud Thinker"
DEVICE_TOKEN_PATH = "/api/v1/login/cli/device/token"
API_ERROR_REF = "#/components/schemas/ApiErrorResponse"


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


def _pin_title(spec: dict) -> None:
    """Pin `info.title` so the snapshot does not depend on the dumping env."""
    info = spec.get("info")
    if isinstance(info, dict):
        info["title"] = SPEC_TITLE


def _keep_typed_device_poll_error(spec: dict) -> None:
    operation = spec.get("paths", {}).get(DEVICE_TOKEN_PATH, {}).get("post", {})
    responses = operation.get("responses")
    if isinstance(responses, dict):
        responses.pop("422", None)


def _response_json_schema(response: object) -> object | None:
    if not isinstance(response, dict):
        return None
    content = response.get("content")
    if not isinstance(content, dict):
        return None
    media_type = content.get("application/json")
    if not isinstance(media_type, dict):
        return None
    return media_type.get("schema")


def _is_generic_api_error(response: object) -> bool:
    return _response_json_schema(response) == {"$ref": API_ERROR_REF}


def _normalize_error_responses(spec: dict) -> None:
    for path_item in spec.get("paths", {}).values():
        if not isinstance(path_item, dict):
            continue
        for operation in path_item.values():
            if not isinstance(operation, dict):
                continue
            responses = operation.get("responses")
            if not isinstance(responses, dict):
                continue
            validation_schema = _response_json_schema(responses.get("422"))
            default_schema = _response_json_schema(responses.get("default"))
            if validation_schema is not None and validation_schema == default_schema:
                responses.pop("422")
            for status in ("default", "4XX", "5XX"):
                if _is_generic_api_error(responses.get(status)):
                    responses.pop(status)


def fixup(spec: dict) -> dict:
    spec = _fix_nullable(spec)
    _fix_exclusive_bounds(spec)
    _dedupe_operation_ids(spec)
    _pin_title(spec)
    _keep_typed_device_poll_error(spec)
    _normalize_error_responses(spec)
    prune_unused_schemas(spec)
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
    print("fixup_spec: applied CLI codegen compatibility fixes")


if __name__ == "__main__":
    main()
