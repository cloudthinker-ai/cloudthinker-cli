## [0.7.2]

- The bundled agent now hides cloud read and write output until you press ctrl+o, so the chat shows what each command is for instead of its raw output.
- `worker start` on a large folder now passes verification, and a failed check shows its reason in the setup dialog.
- The source code and the releases now live in one public repository, https://github.com/cloudthinker-ai/cloudthinker-cli. Download URLs and `cloudthinker update` work as before.

## [0.7.1]

- On macOS, outpost shell commands now run in their working directory instead of failing with `SHELL_UNAVAILABLE`.
- The bundled agent now displays Markdown tables with light horizontal separators and aligned values.

## [0.7.0]

- Run agent background shell commands on an outpost: the worker starts, tails, cancels, and cleans up a detached supervisor that owns the command's output, exit code, and deadline, so a job outlives the operation that started it. A tail may block on the worker for up to 20 s until new output or the exit code appears.

## [0.6.0]

- Serve an outpost from the CloudThinker CLI with outbound broker connections, durable receipts, and target-local connection credentials.
- Worker outposts retry transient heartbeat and long-poll transport failures with bounded backoff.
- The worker installs verified public and custom skill runtime bundles from chunked gateway operations into its private state cache, reuses completed bundles after restart, and exposes their read-only roots to shell operations through `CLOUDTHINKER_SKILL_BUNDLES`.
- The CLI can install, inspect, start, stop, and remove a per-user worker service through systemd user units on Linux or LaunchAgents on macOS. Service descriptors persist only explicit non-secret worker arguments and always load credentials from the private worker store.
- Reject new skill bundle downloads when the worker cache holds 64 installed or staging digests; existing installed bundles remain usable.
- Expose `cloudthinker whoami --json` with the authenticated user, workspace, and host IDs for Desktop integrations.

## [0.5.11]

- Updating from the start-up offer now shows one spinner line, `Updating cloudthinker to <version>`, and one `Updated cloudthinker from <old> to <new>` line, instead of the installer log and a separate agent download line; a failed install still prints the installer output (APT-1028).
- The first `cloudthinker agent` start shows `Setting up cloudthinker <version>` while it downloads, and `cloudthinker update` shows `Checking for updates` while it works.
- The start-up offer no longer waits on GitHub: it reads a local cache that refreshes in the background at most every 20 hours, so a new release is offered on the start after the refresh finds it (APT-1031).
- Answering `s` at the offer skips that version until a newer one ships.
- An update from the start-up offer installs the new agent bundle too, so the start after it downloads nothing.
- `cloudthinker update` installs the new agent bundle only when an agent bundle is already installed, and a bundle failure there is a warning.
- A failed background check retries after one hour instead of waiting 20 hours.

## [0.5.10]

- `cloudthinker agent --help` and `-h` now print the agent's own help, which lists flags such as `--tui-mode fullscreen`, and need no login or update prompt (APT-1017).

## [0.5.9]

- Release builds no longer use GitHub Actions artifact storage: build outputs pass between jobs through `actions/cache` under run-scoped keys, so a stale quota state on GitHub's side can no longer block a release (APT-1002).

## [0.5.8]

- The update channel now follows the origin: the production site offers stable releases, any other `CLOUDTHINKER_URL` origin offers dev prereleases, and the startup offer says when a build is on the dev channel.

## [0.5.7]

- The `chat -p` submit line now says the run executes in your CloudThinker workspace (cloud) and cannot see your local files.

## [0.5.6]

- The published install one-liner now points at `https://cloudthinker.io/install.sh`; the old `cloudthinker.ai` host keeps redirecting.
- Stabilized the agent release probe under parallel load by giving the install hygiene check the production 10-second probe budget.

## [0.5.5]

- Verified staged agent executables before installation and retained the previous bundle.
- Cross-checked agent downloads against both their checksum sidecar and the published agent inventory.
- Preserved bundles when process inspection could not prove they were unused.
- Distinguished rejected stored logins from missing credentials.
- Added opt-in startup phase timing.
- Included subagent delegation in the agent bundle with CloudThinker agent modes and separate session records.
- Headless chat sends the default Pro selection required by the current backend; Starter workspaces retain the server-enforced Light mode.
- Agent installation tolerates a briefly busy staged executable while retaining the existing probe timeout.
