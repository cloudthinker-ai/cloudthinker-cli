# Export and import workspace Policy

Use this module to move a workspace's Policy through a YAML file.
Establish the intended host and workspace with `cloudthinker whoami` first.
Use the same global `--url` and `--workspace` on every call.
Export needs `agent_policy:view`; both preview and apply need `agent_policy:edit`.

## Export

```bash
cloudthinker policy export
cloudthinker policy export --output policy.yaml
```

Stdout contains only YAML when `--output` is absent. There is no export `--json` flag.
`--output` publishes a complete private file and reports its path on stderr.
Its path must be new. Existing files, symlinks, and directories are refused.
Relative paths use the caller's directory; missing parent directories are not created.
The file can include approver emails and policy detail. Keep it private as appropriate.

## Preview

```bash
cloudthinker policy import policy.yaml
cloudthinker policy import policy.yaml --json
cloudthinker policy import policy.yaml --prune --activate --json
```

Import accepts a regular UTF-8 YAML or JSON file. It sends one preview request by default.
Read `mode`, `applied`, `counts`, `rules`, `sections`, and `errors` together.
Preview reports `mode: plan` and `applied: false`; it makes no Policy changes.
Every document issue carries its code, path, nullable location, slug, and allowed values.
An HTTP success with document issues still exits 2. Fix the issues and preview again.

`--prune` dismisses live slugged Rules omitted from the file.
It also clears omitted origin overrides when the file includes `origin_levels`.
It does not remove groups or Rules without slugs.
`--activate` requests active Allow Rules instead of proposals for new or changed imports.
Both flags default to false and can preview their effects without writing anything.

## Apply only authorized changes

```bash
cloudthinker policy import policy.yaml --apply --json
cloudthinker policy import policy.yaml --prune --activate --apply --json
```

Only `--apply` permits a write. A loaded guide or successful preview is not write authorization.
Repeat any intended `--prune` or `--activate` flags on the apply command.
Without activation, new or changed Allow Rules wait as proposals; unchanged active Rules keep their status.
Ask first and Deny Rules become active under the existing server rules.

Apply recomputes the plan against current state under server locks; the earlier preview is not a saved revision.
Any validation issue prevents the transaction from applying. Successful applies use the existing Policy audit trail.
There is no automatic retry of a failed apply. A dropped response can leave the result unknown.
Export and preview again before deciding whether another apply is needed.

Exit 0 means a valid preview or a confirmed apply. Exit 2 means local input or document validation failed.
Exit 3 means login or permission is missing. Exit 1 covers output, transport, conflict, or protocol failure.
An output error after apply does not undo the server's change.
For a conflict, preview current state again. For a forbidden response, ask a workspace admin for access.
