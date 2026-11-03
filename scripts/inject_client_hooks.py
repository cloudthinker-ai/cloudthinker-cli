#!/usr/bin/env python3
"""Append the hand-written `ClientHooks` impl to the generated `cloudthinker-api` lib.rs.

The orphan rule keeps the impl inside the generated crate, and `make gen`
deletes that crate first, so the hook source lives in `client_hooks.rs` and
this step re-appends it. The hook adds `cyber_vocabulary=2` to every
`/api/v1/cyber/**` and `/api/v1/appsec/**` request. Idempotent.
"""

from __future__ import annotations

import sys
from pathlib import Path

HOOKS = Path(__file__).with_name("client_hooks.rs")
MARKER = "impl ClientHooks<()> for Client {"


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: inject_client_hooks.py <lib.rs>")
    path = Path(sys.argv[1])
    body = path.read_text()
    if MARKER in body:
        return
    path.write_text(body.rstrip("\n") + "\n" + HOOKS.read_text())
    print("inject_client_hooks: hooks appended")


if __name__ == "__main__":
    main()
