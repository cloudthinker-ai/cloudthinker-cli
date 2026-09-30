## Additional changes for 0.7.6

- Replace the fixed tour command with a bundled /skill:tour that explains Local and Cloud and helps users choose a first task.
- Limit automatic agent skill metadata to 8 KiB and preserve discovery through a private local index. Include the missing canonical AppSec request helpers in the Discovery runtime bundle.

## [0.7.6]

- Worker `glob_read` reads the head of a large file instead of failing the whole scan, so a big `SKILL.md` no longer hides the Worker folder's Skills.
- On 64-bit Arm Linux, the credential lock and the worker skill bundle cache lock now refuse a symlink instead of following it, as they already did on x86_64 Linux and macOS.
- Reduce worker memory use: file reads, operation dispatch, receipt journaling, and artifact uploads no longer copy operation payloads or file contents; journal bytes and JSON output are unchanged.
- Worker file search, grep, and skill bundle verification do less repeated work on large folders; their results are unchanged.
- A server error or rate limit during a token refresh is retried, honouring `Retry-After`, instead of logging you out; device-code login keeps polling through a server error.
- A refresh keeps your active workspace; it no longer switches to the workspace you last used with `--workspace`.
- An older `cloudthinker` never overwrites a credentials file written by a newer one, and an unreadable credentials file is moved aside before a login replaces it.
- A 403 shows the server's reason and asks for a workspace admin instead of sending you to `cloudthinker login`.
- Every login hint names the host you used, so `cloudthinker --url https://dev.cloudthinker.io ...` tells you to run `cloudthinker login --url https://dev.cloudthinker.io`.
- New `cloudthinker auth status` lists your stored workspace logins, and `cloudthinker auth switch <workspace>` makes one active.
- `logout` says why a server revoke failed, says when nothing was stored, names the stored workspaces when none is active, and warns while `CLOUDTHINKER_TOKEN` is still set.
- `cloudthinker auth token` finishes a token refresh it started before it stops on Ctrl-C, and the agent uses the same `cloudthinker` binary for its token.
- Commands no longer read the OS keyring once the credentials file holds the host's login.
- Worker credential errors say which check failed, and a missing outpost credential asks you to register the outpost again.
- `worker start` keeps running through short CloudThinker outages: it retries server errors and rate limits, reconnects after a laptop sleep or network change, and no longer cancels running work on one bad gateway.
- A finished operation result is retried until its lease ends, so it is no longer marked unknown after five seconds of network trouble.
- A machine clock that runs ahead or behind no longer fails outpost work.
- The worker log names each assignment and operation with its duration and error code, and server errors show their code and request ID.
- The first Ctrl-C drains running operations and says how many are left; a second Ctrl-C cancels them.
- Shell output that was printed before a detached background process kept the output open is now returned instead of lost.
- The skill bundle cache frees bundles unused for 25 hours, so new skills keep installing after 64 versions.
- `worker service status` shows the last exit code and where the logs are, the macOS service now writes a log file, and service manager errors show their reason.
- A replaced served folder stops the worker with a clear message; `worker start --reset-folder` serves the new folder.
- `worker service install` updates a service that an earlier release installed with the same settings and reports `updated`, instead of asking you to uninstall it first.
- `login` over SSH or on Linux without a display now shows a short code to enter in any browser, instead of waiting 5 minutes for a browser callback that cannot arrive. Set `CLOUDTHINKER_LOGIN=browser` to keep the browser callback.
- `login --url <address>` now remembers that address, so later commands and login hints reach the same CloudThinker without `--url`. Logging out of that address returns the default to production.
- `chat -p` now adds piped input to the prompt, for example `kubectl logs pod | cloudthinker chat -p "why does this crash?"`, and `chat -p -` reads the whole prompt from stdin.
- `chat -p` now shows a spinner with the run status and elapsed time on a terminal. Ctrl-C stops the wait and prints the command that resumes it.
- `chat --json` now includes `message` and `failure_kind`, so a script can see why a run failed.
- `review --fail-on <severity>` and `review watch --fail-on <severity>` exit 6 when a finding reaches that severity, so a CI job can gate on a review.
- New `cloudthinker completion <shell>` prints a completion script for bash, zsh, fish, PowerShell, or elvish.
- `chat`, `review`, `login`, `logout`, and `whoami` now end with one line when a newer release exists.
- `whoami --json` now includes the account email and the workspace name, and `chat ls` says so when there are no runs.
- `--help` now opens with a short get-started list, and a failed `update` names the reinstall command.
- Run local Cyber discovery through the shared background command engine; reserve workflows for agent investigation and verification. Discovery cancellation also stops collectors created during cleanup.
- Start local pentests from the interactive agent with automatic setup checks, configuration repair, visible progress, and readable results.
- Add internal Cyber commands for targets, runs, scoped probes, coverage, identities, findings, and evidence. The backend owns the plan and final result.
- Keep coverage readable after a run finishes while preventing further plan or observation changes.
- Add the local development launcher, separating backend and target credentials and keeping token refresh within the agent deadline.
- Omit cloud-only memory paths from local run briefs and report missing coverage reasons before authentication or network calls.
- Keep a private per-run local workspace for resumable evidence drafts and workflow journals; repeated binds reuse it, while canonical App memory remains the cross-run source.
- Match saved target paths using backend trailing-slash normalization without changing query values.
- `cyber memory pull <app-id> --output <dir>` saves the App's canonical findings and surface to a local directory. `--include-context` downloads non-credential App documents with explicit opt-in; context files are bounded and their signed URLs are never written to the snapshot.
- Identify the local CLI build as 0.7.6-dev.1, including background command execution and corrected Cyber workflow guidance.
- Local Cyber workflows run the shared discovery collectors on your machine, preserve discovery limitations, and require a valid report before successful completion.
- Local source-agent runs can use the launcher-selected auth binary for token resolution while the production agent keeps the wrapper's own native binary.
- Keep the CLI API client aligned with password authentication and review job commit counts after the develop merge.
- `cloudthinker-local` defaults workflow concurrency to 4; set `CLOUDTHINKER_WORKFLOW_MAX_CONCURRENCY=8` to use the shipped default. Values must be from 1 to 8.

## [0.7.4]

- Ship the bundled agent with fflate 0.8.3, which fixes an infinite loop on malformed ZIP64 skill archives.

## [0.7.3]

- Review the local checkout with a bundled read-only Pi agent using CloudThinker inference; keep the prompt, transcript, findings, and rendered result local.
- Confine the review agent's file tools to existing, non-ignored paths within the checkout and hide Git metadata and ignored paths from tool results.
- Enforce worker transport retry deadlines even when fetch completion and the deadline become ready before the runtime is polled.

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
