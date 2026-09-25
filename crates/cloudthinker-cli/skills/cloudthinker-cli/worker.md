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

Use `cloudthinker worker status --outpost my-project --json` for availability,
`cloudthinker worker outpost ls --json` to list authorized outposts, and
`cloudthinker worker outpost archive my-project` after active assignments finish.

The CLI stores the scoped worker credential in a private `0600` file. Worker
startup never uses browser login or the OS keyring. For headless operation,
provide `CLOUDTHINKER_WORKER_TOKEN` and the UUID in `CLOUDTHINKER_OUTPOST_ID` through
your process environment. Never put either a credential or enrollment reference
in an agent prompt, log, service descriptor, or source file. A one-use
registration reference from setup can be exchanged with `worker start --register
<reference> --workdir "$PWD"`.

To keep an outpost available after its terminal closes, register a per-user
service. Installation writes a native systemd user unit on Linux or a LaunchAgent
on macOS and does not start it:

```bash
cloudthinker worker service install --outpost my-project --workdir "$PWD" --concurrency 4
cloudthinker worker service start --outpost my-project --workdir "$PWD"
cloudthinker worker service status --outpost my-project --workdir "$PWD" --json
```

The descriptor stores the executable, host, workspace, outpost UUID, workdir,
concurrency, and label as arguments. The worker reads its credential from the
private store for that host; service install and start require that stored
credential, so the descriptor and service manager logs contain no token. `stop`
sends `SIGTERM` and lets the worker drain current assignments;
`uninstall` stops and removes the service while retaining the credential,
outpost, and workdir. A service is keyed by host, outpost, and canonical workdir;
installing different settings for the same key requires uninstalling first.

The Linux unit is scoped to `systemd --user`, so closing a terminal does not stop
it. A user manager still needs lingering enabled separately to run after logout
or reboot. The macOS LaunchAgent loads for the logged-in user at login; it is not
a pre-login system daemon. The commands report a clear unsupported-manager error
on other operating systems.

The default shell environment contains PATH, HOME, LANG, TERM, USER, and TMPDIR.
Add individual variables with `--env NAME`; use `--inherit-env` only when the
whole environment belongs to the work being served. Worker credentials and shell
startup injection variables are always excluded.

Press Ctrl-C or send SIGTERM to stop claiming assignments and drain current
work. Upgrade the CLI after it exits, then run the same command. Worker startup
never offers an automatic update. Reusing the directory preserves its local
identity and files; replacing the directory fails with `WORKDIR_IDENTITY_CHANGED`.

Concurrency limits assignments. Each assignment admits four operations and the
process executes at most 16 operations at once. File reads and uploads allow
1,400,000 bytes per file; output uploads allow 64 files and 16 MiB per operation.
Searches bound both bytes read and result size and report truncation. Large files
must be split or reduced before transfer.

An uncertain mutation stays unresolved. Check the conversation and outpost
status; do not repeat a command to guess whether it succeeded. The worker keeps
unacknowledged receipts locally for reconciliation and retires acknowledged
journals only after the server accepts release.

Skill bundles use a private cache outside the project directory. The cache admits
up to 64 installed or staging digests. At capacity, new downloads fail with
`BUNDLE_CACHE_FULL`; installed bundles remain usable. The worker never deletes a
bundle that a running command may still reference.
