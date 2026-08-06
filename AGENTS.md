# CloudThinker CLI

Customer-facing `cloudthinker` binary (browser or device-code login + headless `chat -p`) as a Cargo workspace; a thin job-runner over the backend's CLI endpoints. Concept: [[concepts/cli/README]].

## Boundaries

### ALWAYS

- Regenerate the wire crate with `make -C cli gen` after the backend's CLI endpoints change; commit both `openapi/cloudthinker-cli.json` and `crates/cloudthinker-api/`. Never hand-edit `cloudthinker-api` — it is generated.
- Keep all wire + auth in `cloudthinker-client`; commands go through it and never import `reqwest`/`serde_json` directly.
- Build a serializable output and render it through the one `engine/output.rs` helper — no per-command `--json` branch.
- Return an `engine/exit.rs::ExitCode`; never call `process::exit`. The exit-code table is a stable contract (a test pins it).
- `chat -p` writes ONLY the answer to stdout; progress/status/errors go to stderr (a test pins stdout purity).
- Pass `make -C cli check` (fmt + clippy `-D warnings` + tests) before done; every `CA-CLI-*` case keeps a traced test.

### NEVER

- Add a workspace lint opt-in (`[lints] workspace = true`) to `cloudthinker-api`; it stays relaxed-lint (generated).
- Change the reqwest major/features out of lockstep with what `make gen` emits — the client hands its `reqwest::Client` to the generated `Client`, so a skew breaks the type.
- Touch the real OS keyring in tests; use `MockTokenStore` / a boxed keyring double.
- Bypass the refresh single-flight + guarded disk reload — concurrent refresh trips the backend's token-family reuse detection.

### ASK FIRST

- Adding a new command, crate, or backend endpoint to the OpenAPI allowlist (`scripts/prune_spec.py`).
- Changing the default base URL or the exit-code mapping.

## Architecture

Three crates: `cloudthinker-api` (progenitor-generated from a pruned OpenAPI snapshot), `cloudthinker-client` (thiserror; PKCE login, workspace-keyed token store/refresh, typed `CtClient`), `cloudthinker-cli` (anyhow-free clap dispatch + `engine/{output,watch,exit}`). Authenticated commands accept global `--workspace <id|name>`; `whoami` proves the resolved live identity. `make gen` = dump spec → `prune_spec.py` → down-convert 3.1→3.0 → `fixup_spec.py` → progenitor → inject relaxed-lint header; idempotent.

## Gotchas

- The generated client already includes `/api/v1` in each path, so the base URL passed to it is the bare origin (e.g. `https://app.cloudthinker.io`), not `{base}/api/v1`.
- Generic API errors stay undeclared in the pruned CLI schema so progenitor returns `UnexpectedResponse` with the real HTTP status during the detail-only compatibility window. `error.rs` reads either `error.message` or legacy `detail`; declared bodies that fail to decode remain `CtError::Protocol`.
- The device-token poll keeps its RFC-shaped 400 body but drops the generic 422 during spec pruning because progenitor supports one typed error body per operation; run `make -C cli gen` twice after auth schema changes to prove generation is idempotent.
