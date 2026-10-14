# Worker outposts

An outpost lets a CloudThinker conversation work in a directory on your machine.
Install the CloudThinker CLI on Linux or macOS and select the intended host and
workspace with `cloudthinker whoami`.

```bash
cloudthinker worker outpost create my-project
cloudthinker worker start --outpost my-project --workdir "$PWD" --concurrency 4
```

Creation makes a personal outpost. Add `--shared` to make it available to your
workspace; that requires workspace settings permission. Select the outpost when
creating a conversation. Existing conversations keep their original target.

The worker connects outward over HTTPS and opens no inbound port. Startup checks
file access, shell execution, cancellation, and artifact upload before the outpost
becomes available. File tools stay inside the selected directory. Shell commands
run under your OS account with `/bin/bash` and are not sandboxed.

CloudThinker records each shell command, background task start or stop, and file
write in the Audit Trail under the `Command` filter. The record keeps a redacted
preview of the command, at most 1 KiB, and never the output. Reads are not recorded.

Use `cloudthinker worker status --outpost my-project --json` for availability
and the worker CLI version. It adds `Update available` when the server
recommends a newer CLI, or `Outdated` when the CLI is below the server minimum.
Run `cloudthinker update` to fix both. Use
`cloudthinker worker outpost ls --json` to list authorized outposts, and
`cloudthinker worker outpost archive my-project` after active assignments finish.

The CLI stores the scoped worker credential in a private `0600` file. Worker
startup never uses browser login or the OS keyring. For headless operation,
provide `CLOUDTHINKER_WORKER_TOKEN` and the UUID in `CLOUDTHINKER_OUTPOST_ID` through
your process environment. Never put either a credential or enrollment reference
in an agent prompt, log, service descriptor, or source file. A one-use
registration reference from setup can be exchanged with `worker start --register
<reference> --workdir "$PWD"`.

A credential stays valid for 30 days after its last use, so a worker that keeps
running never expires. About every 25 days the server asks the worker to rotate;
a worker that read its credential from the private store gets a new one, saves it,
and keeps serving with no action from you. A credential from
`CLOUDTHINKER_WORKER_TOKEN` is renewed by use but never replaced, so replace it
yourself from the Workers page. If a worker prints "This Worker's credential
expired or was revoked" and exits with code 3, enroll it again with the command
from the Workers page. The systemd unit does not restart on exit code 3. A
LaunchAgent cannot skip that restart, so unload it and enroll again.

Pass `--label` to `worker start` to name the served folder. The Workers page
shows the label for each worker.

To keep an outpost available after its terminal closes, register a per-user
service. Installation writes a native systemd user unit on Linux or a LaunchAgent
on macOS and does not start it:

```bash
cloudthinker worker service install --outpost my-project --workdir "$PWD" --concurrency 4
cloudthinker worker service start --outpost my-project --workdir "$PWD"
cloudthinker worker service status --outpost my-project --workdir "$PWD" --json
```

`--workdir` defaults to the current folder, and `--outpost` to the only outpost
set up on this machine, so from the served folder `cloudthinker worker service
start` needs no flags; with two or more outposts it asks for `--outpost`.

The descriptor stores the executable, host, workspace, outpost UUID, workdir,
concurrency, and label as arguments. The worker reads its credential from the
private store for that host; service install and start require that stored
credential, so the descriptor and service manager logs contain no token. `stop`
sends `SIGTERM` and lets the worker drain current assignments;
`uninstall` stops and removes the service while retaining the credential,
outpost, and workdir. A service is keyed by host, outpost, and canonical workdir;
installing different settings for the same key requires uninstalling first.

`status` reports the last exit code of a stopped or failed service and where its
logs are: `journalctl --user -u <unit>` on Linux, and a private
`logs/<service>.log` file in the worker state directory on macOS.

The Linux unit is scoped to `systemd --user`, so closing a terminal does not stop
it. A user manager needs lingering to run after logout or at boot. `service
install` checks it and prints `loginctl enable-linger <user>` when it is off;
add `--enable-linger` to turn it on. The macOS LaunchAgent loads for the
logged-in user at login; it is not a pre-login system daemon. The commands
report a clear unsupported-manager error on other operating systems.

For a machine-wide service, enroll the worker as the account that will run it
(`worker start --register <reference> --workdir <folder>`), then run as root:

```bash
sudo cloudthinker worker service install --system --user alice --outpost <outpost-uuid> --workdir /srv/project
sudo cloudthinker worker service start --system --user alice --outpost <outpost-uuid> --workdir /srv/project
```

`--system` writes a systemd system unit with `User=` on Linux or a LaunchDaemon
with `UserName` on macOS. The credential stays in that account's `0600` store.
Pass the outpost id, not its name. Install the CLI in a path the account can run,
such as `/usr/local/bin`.

A conversation can use its own git worktree. Start the conversation with **New
worktree** on and pick a base branch; the folder you serve must be a git
repository and the worker needs git 2.38 or newer. On the first operation the
worker runs `git worktree add -b cloudthinker/<id> <state>/worktrees/<id> <base>`
outside the served folder, so your main checkout stays unchanged. It uses the
local branch if it exists, and otherwise fetches `origin/<base>` once. A missing
branch fails with `EXECUTOR_WORKTREE_UNAVAILABLE`; fetch the branch on the machine
or pick another one, and the conversation stays bound. A worker that predates
worktrees never receives these conversations: update it with `cloudthinker update`.
The worker removes a worktree after 14 days without use, only when it has no
uncommitted changes, and deletes its `cloudthinker/<id>` branch only when that
branch is merged or pushed. List them with `cloudthinker worker worktrees ls
[--outpost <name>] [--json]` and run the same cleanup now with `worker worktrees
prune`. Dirty worktrees stay and show as `dirty`.

The default shell environment contains PATH, HOME, LANG, TERM, USER, and TMPDIR.
Add individual variables by name with `--env NAME`; the value comes from the worker's own environment, so `NAME=VALUE` is not accepted; use `--inherit-env` only when the
whole environment belongs to the work being served. Worker credentials and shell
startup injection variables are always excluded.

Press Ctrl-C or send SIGTERM to stop claiming assignments and drain current
work; the worker prints how many operations are still running. A second Ctrl-C
cancels them and reports them as cancelled. Upgrade the CLI after it exits, then
run the same command. Worker startup never offers an automatic update. Reusing
the directory preserves its local identity and files; replacing the directory
stops the worker with `WORKDIR_IDENTITY_CHANGED`. Run `worker start` once with the
same `--outpost` and `--workdir` plus `--reset-folder` to serve the new directory;
conversations that used the old directory stay unavailable.

The worker survives short outages. It retries server errors and rate limits,
reconnects after sleep or a network change, and reads deadlines against the
server clock. It stops only when its credential is rejected, the folder is
replaced, or its local state is unusable. Each assignment and operation is logged
with its duration and error code; set `CLOUDTHINKER_WORKER_LOG=debug` to also log
operation starts and assignments another worker claimed first.

An admin can turn Workers off for the whole instance or for one workspace. The
server then answers `EXECUTOR_OUTPOSTS_DISABLED` (HTTP 403) to a worker. A
worker does not exit: it prints one line, checks every 60 seconds, and prints one
line when Workers are on again. Work already running reaches its deadline. In a
conversation bound to a Worker, new operations fail with the same code, and the
agent must tell the user that Workers are off. It must not retry or move the work
to CloudThinker Cloud. An older CLI exits on this code, so run
`cloudthinker update`.

Pass `--log-format json` to `worker start` for one JSON object per line on stderr,
with the fields `ts`, `level`, `event`, `assignment_id`, `operation_kind`,
`outcome`, `duration_ms`, and `cli_version`. These lines never hold scripts, paths,
or output. `CLOUDTHINKER_WORKER_LOG=debug` still adds the debug events.

Pass `--health-addr 127.0.0.1:9464` to serve two read-only endpoints. `/healthz`
answers 200 only when the last poll succeeded less than 60 seconds ago and the
credential is accepted; otherwise it answers 503. Its JSON body holds `state`,
`last_poll_age_ms`, `active_assignments`, `max_assignments`, `cli_version`,
and `credential_expires_at`. `/metrics` serves Prometheus text:
`cloudthinker_worker_info`, `cloudthinker_worker_active_assignments`,
`cloudthinker_worker_capacity`, `cloudthinker_worker_poll_errors_total`,
`cloudthinker_worker_operations_total`, and
`cloudthinker_worker_operation_duration_seconds`. Keep the address on loopback; the
endpoints have no authentication.

Run `cloudthinker worker doctor [--outpost NAME] [--workdir DIR] [--json]` before
the first start and after any outage. It checks the CLI version, the HTTPS path
to the server (it names `HTTPS_PROXY` and a custom CA bundle when set), clock skew
(warning over 10 seconds, failure over 30, because leases last 60 seconds), the
stored credential, the served folder and its saved identity, the state directory
modes (`0700` and `0600`), the service unit and Linux lingering, and the `git`
version. Each
problem prints one fix, and the exit code is 1 when any check fails.

Concurrency limits assignments. Each assignment admits four operations and the
process executes at most 16 operations at once. File reads and uploads allow
1,400,000 bytes per file; output uploads allow 64 files and 16 MiB per operation.
Searches bound both bytes read and result size and report truncation. Large files
must be split or reduced before transfer.

An uncertain mutation stays unresolved. Check the conversation and outpost
status; do not repeat a command to guess whether it succeeded. The worker keeps
unacknowledged receipts locally for reconciliation and retires acknowledged
journals only after the server accepts release. A journal left untouched for
seven days is removed.

Skill bundles use a private cache outside the project directory. The cache admits
up to 64 installed or staging digests. At capacity, the worker frees the least
recently used bundle that was not installed or reused for 25 hours, longer than any
command or background task can run. When no bundle is that old, new downloads fail
with `BUNDLE_CACHE_FULL`; installed bundles remain usable.
