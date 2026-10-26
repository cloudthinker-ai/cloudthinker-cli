# Check and repair workspace Connections

Use this module when the user asks which Connections a workspace has, whether one works, why its test failed, or how to add one. Establish the intended identity with the auth module first, and pass the same global `--url` and `--workspace` on every call.

`connections` manages a Connection's lifecycle by id and test status. `cloud connections` is different: it lists the prefixes and skills your own agent can execute through.

## Read

```bash
cloudthinker connections list --json
cloudthinker connections show <target> --json
cloudthinker connections status <target> --json
```

A `<target>` is a connection id, alias, or prefix. An id is safest: when a prefix matches several rows, the command exits 2 and prints the candidate ids. Pick one with the user; never guess production. Exit 1 with "no connection matches" means this workspace has none; ask whether another workspace was intended.

`list` returns only Connections added to the workspace, never the catalog. Output never carries credentials, environment values, or headers. Read `status` (`connected`, `testing`, `disconnecting`, `error`) and `status_message`. A newer server can send another value; report it as given.

## Follow a test

```bash
cloudthinker connections status <id> --wait --timeout 300 --json
```

`phase` ends as `succeeded`, `failed`, `timeout`, or `cancelled`; any other phase means the test is still running. Exit 0 means the latest test succeeded or is still running without `--wait`; exit 1 means it failed, with `last_error` in the JSON and the reason on stderr. Exit 4 means your wait ended, not the test; resume with the printed `status <id> --wait` command.

## Retry

```bash
cloudthinker connections retry <id> --wait --json
```

Retry re-tests a built-in Connection with the credentials it already stores; it needs a workspace admin, and exit 3 says access is missing. It does not retry an MCP Connection (exit 2). An OAuth Connection cannot retry (exit 1); give the user the `connect-url` link to reconnect. Exit 1 "already testing" means a test is running: follow it with `status --wait` instead of retrying again.

## Add or reconnect

```bash
cloudthinker connections connect-url <prefix> --json
cloudthinker connections connect-url <prefix> --wait --json
```

The command prints the web page that adds that Connection type in the selected workspace, and opens it on a desktop. Give the URL to the user: they enter credentials in the browser. Never ask for, accept, or pass a credential through the CLI or chat. `--wait` returns once a new Connection of that type appears and its first test ends, with `connection` and `test` in the JSON. An unknown prefix exits 2.
