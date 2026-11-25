# Call the CloudThinker public API

Use this module to read or change workspace data through the public API: incidents and signals, reviews and releases, cost findings and anomalies, Cyber apps and findings, Connections, runs and pending approvals.
Establish the host and workspace with `cloudthinker whoami` first.

## Find the operation

```bash
cloudthinker api --list          # command, operation name, method, permission
cloudthinker api --list --json
cloudthinker resolve incidents list --help
```

Operation names are `module.resource.verb`, and the command is the same words: `resolve.incidents.signals` is `cloudthinker resolve incidents signals <incident-id>`, `cyber.branches.findings` is `cloudthinker cyber branches findings <branch-id>`. `token.get` (`cloudthinker token get`) shows what the current token can reach and needs no workspace. `cloudthinker api <operation>` runs the same thing by name; `review.findings.get` is `cloudthinker review findings <finding-id>`, the same command that lists a merge request's findings from its URL.

## Read

Set `WORKSPACE_ID` and `REVIEW_ID` to the IDs you want to use.

```bash
cloudthinker resolve incidents list --status OPEN --severity critical
cloudthinker resolve incidents list --all            # follow every page, at most 10,000 items
cloudthinker review reviews get "$REVIEW_ID"
cloudthinker api GET "/v1/workspaces/$WORKSPACE_ID/optimize/findings" --query status=pending
```

- IDs are UUIDs and go in order as positional arguments.
- `-w <workspace-id>` picks the workspace; without it the login's workspace is used.
- Output is the response JSON on stdout. A list is `{items, next_cursor}`; pass `--cursor <next_cursor>` for the next page, or use `--all`.

## Approvals

```bash
cloudthinker approvals list                          # pending requests you can answer, oldest first
cloudthinker approvals list --kind runbook-execution --json
cloudthinker approvals watch <request-id> --json     # one JSON line now, one when a person decides
```

- `list` pages 20 at a time; pass `--cursor <next_cursor>` with the same filters for the next page.
- `watch` checks every 5 seconds (`--interval`) until the request is approved, declined, expired or superseded. Exit 0 means approved; exit 1 means it closed without approval; exit 4 is `--timeout`; Ctrl-C (130) leaves the request open.
- A person approves or declines in CloudThinker at the request's `url`. The CLI cannot answer a request; never tell the user it can.
- JSON fields the server does not report yet (`approver`, `deliveries`, `advisor`, `decision`, ...) are `null`, not missing. `source_status` keeps the server's own status.

## Write

Set `INCIDENT_ID` to the incident ID. After the first command, set `ETAG` to the value it prints, including the quotes if shown.

```bash
cloudthinker resolve incidents get "$INCIDENT_ID"
cloudthinker resolve incidents update "$INCIDENT_ID" --set status=RESOLVED --etag "$ETAG"
cloudthinker chat runs create --body @run.json --key retry-1
```

- `--body` takes JSON text, `@file` or `-` for stdin; `--set key=value` sets one top-level field.
- Every write carries an `Idempotency-Key`. Pass your own with `--key` and reuse it to retry an uncertain write safely; never retry a write without it.
- `412` means the resource changed since you read it: get it again, check the change still makes sense, then retry with the new ETag.
- A write runs when the token's role and the user's current workspace role both allow it. An operation that asks first (every delete by default) refuses with exit 2 when no terminal can answer. Confirm the exact action with the user, then run it again with `--yes`.

## Errors

Exit 3: not logged in, or the token's role or the user's current role lacks the operation's permission. Exit 2: bad arguments or a `422`. Exit 1: not found (`404`, also for another workspace's IDs), rate limit (`429`, wait for `Retry-After`) or a server error. Quote the `request` id from the error when you report a failure.
