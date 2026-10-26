## [0.10.0]

- `cloudthinker worker` now returns images when the agent reads a PNG, JPEG, GIF or WEBP file on an Outpost, so the agent can see screenshots it took.
- The shell installer now asks once whether to use `ct` as a shortcut for `cloudthinker`, before login. Choose Yes for a persistent Bash, Zsh, or Fish alias, then open a new terminal. Enter keeps `cloudthinker`. Existing `ct` commands and shell definitions stay unchanged; updates and automated installs skip the prompt.
- New `cloudthinker connections` command. `connections list` shows the Connections added to your workspace, and `connections show <target>` shows one by id, alias, or prefix, without any credential. `connections status <target> --wait` follows its latest test and exits 1 when the test fails, with the reason. `connections retry <target>` tests a built-in Connection again with its saved credentials (workspace admins). `connections connect-url <prefix>` prints the page that adds a Connection in your browser, opens it on a desktop, and with `--wait` follows the new Connection's first test.
- `cloudthinker worker` can run a conversation's shell and background commands in a Linux Landlock sandbox. A sandboxed command reads the served folder, system folders and the tools on your `PATH` (checked by real path; the folder above a `bin` folder only for known version managers and runtimes), writes only `.cloudthinker-sandbox` and the `runs`, `warm`, `sessions` and `recipes` folders of `.cloudthinker-loupe` (with a private home and temporary folder there), never `/tmp`, other terminals, or the checkout's own files and `.git`, and cannot read the worker credential, `~/.ssh`, `~/.aws`, `~/.config`, or `~/.kube`. It cannot use the kernel keyrings or open UNIX sockets (pipes from `socketpair` still work), so it cannot reach the user's D-Bus, Docker, or SSH agent. The network stays open, including a cloud metadata service, so do not serve Loupe from a host whose instance role it must not use. It gets a short allowlist of environment variables, including proxy and CA certificate settings, plus `CLOUDTHINKER_AGENT_BIN` naming the installed agent and the data folder of asdf, Volta, pyenv, rbenv or mise when that tool is on your `PATH`, so their shims keep working. A worker never offers the sandbox while it serves your home folder or a folder inside a private one, and offers it only on Linux 6.2 or newer with Landlock enabled, and refuses a conversation that needs the sandbox when it cannot provide one. `worker doctor` reports this in a new "Sandbox (Loupe)" row.
- This release also ships the unpublished 0.9.3 notes.
- Cyber commands that get a wrong command or wrong arguments now list the valid commands and suggest the closest one, so the agent fixes the call instead of reading source.
- The pentest agent can mark all remaining coverage rows as done in one step, and a batch with a few rows from another run now records the valid rows with a warning instead of failing.
- The sandbox reads only the git folders the served folder used when `worker start` began, so an edit under `.git` cannot open other folders to it. On Linux 6.12 or newer, a sandboxed command cannot signal processes outside the sandbox. Tools installed with Homebrew in your home folder, `uv tool`, or `pipx` run inside the sandbox.
- `connections connect-url --wait` also finishes when you reconnect a Connection in error.
- On an Outpost, reading an image larger than 1.4 MB fails with `IMAGE_TOO_LARGE`, so the agent knows to shrink it, and an image result carries no convention files.
- The `ct` question does not stop the installer when zsh takes more than five seconds to start.

## [0.9.3]

- `cloudthinker worker start --background` runs the worker as a background service in one step and turns on Linux lingering. The Worker console prints `Waiting for work.` after each run and after a passed folder check, and its banner names `--background`.
- The install one-liner now opens the login right after it installs `cloudthinker` in a terminal, unless you are already logged in. `cloudthinker login` asks how you want to log in: with the browser, or with a code for SSH and remote sessions. Use the arrow keys or press 1 or 2. While login waits, press c to copy the login URL: the CLI uses the system clipboard on your machine, and asks your terminal to copy it (OSC 52) over SSH. `--device-auth` and `CLOUDTHINKER_LOGIN` still skip the question, and `CLOUDTHINKER_CLI_NO_LOGIN=1` skips the login during install.
- `worker start` now shows the folder check as one line, `Checking that agents can work in this folder…`, and then `Folder verified in <time>. Start a chat in CloudThinker to work here.` Its test commands, including one that times out on purpose, no longer print as run failures.
- When the folder check fails, `worker start` now says why, for example `Folder check failed: The folder was too large to check.`, and what to do next.

## [0.9.2]

- `cloudthinker review` now sends the change to the local agent on stdin, not in the command line, and it no longer runs the git filter programs a checkout configures. A stray request to the login callback no longer cancels `cloudthinker login`. A project `.cloudthinker/config.toml` can name only `CYBER_<ROLE>_AUTH` variables as Cyber identities; save any other name with `cyber config set --user`. As root, `worker service` runs `systemctl`, `loginctl`, `launchctl`, `getent`, and `dscl` only from the system folders.
- Harden the CLI against a hostile server, repository, or local user. Login no longer follows a redirect to another site with your tokens, and server replies, downloads, and archives have size limits. Credentials, `cyber memory pull` output, and the project Cyber config stay private and refuse symlinks. A Cyber probe now sends your configured identity only to the run's target address. Server text loses terminal control characters before the CLI prints it. `worker service install --system` refuses a program that another user can replace. The TLS certificate checker also moves to its patched release.

## [0.9.1]

- Stop trusting the compromised eMudhra certificate roots (emSign ECC Root CA - G3, emSign Root CA - C1, emSign ECC Root CA - C3) and the Comodo AAA root that cross-signs them. Every CLI request, `cloudthinker update`, local Cyber discovery, and the bundled agent (now built with Bun 1.4.4) reject a forged HTTPS certificate from these chains, even when the operating system still trusts them. Other roots from the operating system trust store keep working. If you still run 0.8.3 or older on an untrusted network, update your operating system's CA certificates before you update the CLI.
- 0.9.1 is the first published release after 0.8.3, so it also ships the 0.9.0 changes: `cloudthinker remote` runs the agent on your remote machine with one command, and the TLS and HTTP/2 libraries move to their patched releases.
- Keep the packaged Cyber discovery runtime in sync with canonical finding review helpers. Cloud Pentest uses Recon, Hunter, Verifier and Critic profiles while Recon is centralized under the single `cyber-recon` ID; saved Recon children with earlier guidance digests may need a fresh assignment. Critic returns its review through ordinary child completion.
- Keep CLI workers compatible with backend file-operation envelopes used during first-time workspace-memory setup.
- Add paging to Cyber result lists and clarify identity checks and evidence repair.
- `worker start` prints calmer output. A header names the Worker, its folder, the server, the no-sandbox warning, and how to stop it. Each run prints one start line and one summary line with its duration and its shell commands, file reads, and file edits. A failed or timed-out command gets its own plain line. A lost connection prints one line, and `Reconnected.` follows. Ctrl-C prints on its own line and says how many commands still run. Times are short local times. Add `--verbose` (`-v`) to see each operation.

## [0.9.0]

- `cloudthinker remote` runs the agent on your remote machine with one command: name the machine once (`cloudthinker remote vm`), then a bare `cloudthinker remote` returns to it and picks the folder for you, or shows one short list of running agents and recent folders. Ctrl+Z detaches and the agent keeps running; Ctrl+D, Ctrl+C twice, and `/quit` first ask whether to detach or quit, so leaving never kills work by accident. When the network drops, a banner says it is reconnecting and the same screen comes back; Ctrl+V pastes a laptop image, dragged files are copied over, and the remote machine needs nothing besides `cloudthinker`.
- Update the TLS and HTTP/2 libraries the CLI uses for every request to their patched releases. They fix a TLS downgrade, a TLS secret that debug output could reveal, and crashes or memory growth a malicious server could trigger.

## [0.8.3]

- Regenerated the CLI API schema and bundled Cyber discovery runtime so Ultra pentest runs accept their manifest.
- Build against the latest API schema. `cloudthinker review` now shows a review that waits in the queue as `queued`, and Cyber observations whose reason, evidence ref, or worker is too long stop with a clear message before upload.
- A Worker on macOS now keeps serving its folder after a restart. macOS can renumber a disk at boot, and before this fix `worker start` then stopped with `WORKDIR_IDENTITY_CHANGED` and asked for `--reset-folder`, although the folder was the same.

## [0.8.2-dev.4]

- A conversation worktree whose `git worktree add` timed out or was stopped part way is now removed and created again on the next message. Before, the Worker used the half-built folder, where most tracked files looked deleted, and the folder was never pruned.
- Build against the latest API schema, which adds the workspace approval mode. The CLI reads none of these fields, so nothing changes for you.

## [0.8.2-dev.3]

- Consolidated CLI output failure handling across Cyber, Cyber config, incidents, and recommendations without changing command output or exit codes.
- Isolated worker service account and credential resolution without changing system-scope selection or service behavior.
- Moved local Cyber discovery subprocess supervision into a private module without changing its output, timeout, cancellation, or descendant cleanup behavior.
- Refactored worker credential source resolution and rotation into a private module without changing command behavior.
- Moved Cyber API types, conversions, and `CtClient` methods into a feature adapter while preserving their public paths and wire behavior.
- Keep agent release checksum verification and archive extraction behind a verified archive value; install behavior stays unchanged.
- Moved Review API types, conversions, finding ordering, and lookup into a feature adapter while preserving the client facade and wire behavior.
- Cyber agents can choose curl, Python, or Browser Use for requests, use the supplied bearer or session auth, and save screenshots with `cyber evidence`.
- `cloudthinker chat` now treats a cancelled run as terminal and exits with a failure status.

## [0.8.2-dev.2]

- A worker credential now stays valid for 30 days after its last use. A worker that read its credential from the private store rotates it about every 25 days and keeps serving, with no action from you. A `CLOUDTHINKER_WORKER_TOKEN` credential is renewed by use and never replaced.
- An expired or revoked worker credential now stops `cloudthinker worker start` with exit code 3 and the message "This Worker's credential expired or was revoked. Enroll it again with the command from the Workers page." The systemd service does not restart on exit code 3.
- `cloudthinker worker start --label` now sends the label to CloudThinker, so the Workers page shows it.
- `cloudthinker worker start` waits when an admin turns Workers off for the instance or the workspace. It prints one line, checks every 60 seconds, and resumes by itself when Workers are on again, so you do not reinstall or restart it.
- `cloudthinker worker status` shows the worker CLI version and the badge `Update available` or `Outdated` with the fix `cloudthinker update`.
- `cloudthinker worker start` gains `--log-format json` for one JSON log object per line, and `--health-addr` to serve `/healthz` and Prometheus `/metrics` on a local address. `cloudthinker worker doctor` checks the CLI version, network path, clock skew, credential, folder, state modes, service, and git, and prints one fix for each problem.
- A Worker can run each conversation in its own git worktree. When you start a conversation with **New worktree** on, the worker runs `git worktree add -b cloudthinker/<id>` from the branch you pick, in its state folder, so your main checkout stays unchanged. The worker needs git 2.38 or newer, and it reports its branches to CloudThinker.
- `cloudthinker worker worktrees ls` lists the worktrees on this machine, and `cloudthinker worker worktrees prune` removes the ones unused for 14 days that have no uncommitted changes. Add `--outpost` to pick one outpost and `--json` to print JSON. The worker also does this cleanup by itself.
- `cloudthinker worker service install` on Linux checks lingering and tells you when it is off, because a user service then stops at logout and does not start at boot. Add `--enable-linger` to turn it on.
- `worker service install --system --user <account>` run as root installs a machine-wide systemd unit or macOS LaunchDaemon that runs the worker as that account. The credential stays in the account's private store. Enroll the worker as that account first.
- The "worker service directory is unsafe" error now names the directory, its mode, and the `chmod` that fixes it.
- `worker start --env` help now says to give a variable name only.

## [0.8.2-dev.1]

- `cloudthinker worker` sends its CLI version in the `X-CloudThinker-CLI-Version` header on every worker request. A server that sets a minimum CLI version rejects an older worker at registration and heartbeat, and the worker prints the server message and exits instead of retrying; run `cloudthinker update`, then start the worker again.
- A worker response that does not match the protocol now tells you to run `cloudthinker update`.

## [0.8.1]

- First stable release since 0.7.8. It ships every change listed under 0.8.0 and 0.8.0-dev.3 to 0.8.0-dev.5 below, plus these:
- Package stable Cyber discovery in the local CLI runtime. Fence each browser redirect to the frozen scope, and report `PARTIAL` when Playwright is unavailable or a redirect cannot be completed safely.
- Keep the generated CLI API client aligned with the current backend request and response schemas.
- Build again against the latest generated API client (Cyber JEV and Pentest advisor fields, App list sort, selected-finding export); the CLI sends none of them, so the server defaults apply.

## [0.8.0-dev.5]

- Cyber uses canonical command, helper and profile names, `CYBER_*` settings and `cyber/` roots; retired runtime aliases are removed.
- Review status and watch accept stopped reviews and finish waiting when a review stops.
- Refresh the bundled Cyber discovery runtime after executor command consolidation. Discovery uses the canonical Cyber command for cloud surface publication; local discovery retains its existing supervisor and output contract.
- Use canonical Cyber helper packages, `CYBER_*` executor variables and `cyber/` workspace roots; remove old-name runtime forwarding and fallback support.
- Package shared Cyber observation identity and intake dependencies with the canonical discovery runtime. Preserve local issued-plan authority while cloud observations automatically reconcile coverage and retry incomplete assignments.

## [0.8.0-dev.4]

- The CLI names its agent CloudThinker Agent everywhere: `agent` starts CloudThinker Agent in this folder, and `chat` asks CloudThinker Agent in the cloud.
- `cloudthinker incident ls` lists the workspace's incidents, and `cloudthinker incident status <INCIDENT_ID> --wait` polls until the incident is resolved, dismissed, or not found, so an on-call script or CI job can wait on it. It exits 4 on timeout; add `--json` for a script to read.
- `cloudthinker recommendations ls` lists Optimize recommendations with their monthly savings, filtered by `--status`, for cost reports.
- `cloudthinker chat -p` accepts `--agent` for an authorized custom-agent UUID or alias and keeps model selection separate.
- Keep the generated CLI session and capability contract aligned with custom-agent selection and the specialist flag.
- `cloudthinker incident status --help` now separates the exit codes: 0 when the incident closes (including the NOT_FOUND status), 1 when no incident has that ID.
- The bundled Cyber discovery runtime accepts `browser_resource_origins` in a target scope, matching the backend.
- The CLI's API contract no longer carries the incident declaration actor, which the CLI never reads.

## [0.8.0-dev.3]

- The agent opens fullscreen with a status line for workspace, approval mode, credits, and Connections; Ctrl+O opens a searchable transcript, and a cloud write asks in one approval modal.
- A bare `cloudthinker --tui-mode regular` or `--tui-mode fullscreen` picks the agent screen without the `agent` subcommand.
- `cloudthinker agent --help` shows CloudThinker's own help, and the top-level help calls it the CloudThinker agent. Flags and commands the agent does not support now stop with the next step instead of running.
- In the agent, `/share` copies the conversation link, `/login` and `/logout` sign in and out of CloudThinker, `/changelog` shows the agent's releases, and `/bug` opens a GitHub issue.
- The generated API client picks up the code review `layer_count` field.
- `cloudthinker --help` lists what a person runs. Commands only an AI agent drives (`cloud`, and the Cyber run, probe, discovery, and memory steps) still work but are hidden; `cloudthinker --skill <module>` documents them.
- Every command and option now has help text, Cyber help is written for users instead of describing internals, and every `--json` reads the same.
- `chat` is described as the cloud agent and `agent` as the agent in this folder, so the two headless paths (`chat -p` and `agent -p`) are easy to tell apart.
- `login --help` shows one sign-in option, `--device-auth`, for a short code; `--no-browser` still works but is no longer listed.
- `worker service` commands default `--workdir` to the current folder and `--outpost` to the only outpost set up on this machine.
- `cloudthinker agent --help` prints the agent's help at once, without downloading the agent.

## [0.8.0]

- Add direct cloud commands for independent agents: create sessions, discover Connections, load their guides and SDK schemas, execute scoped scripts, and recover task output or approved writes. Print release-matched guidance with `cloudthinker --skill cloud`.

## [0.7.7]

- Render completed Mermaid flowcharts as themed terminal diagrams with authored node border colors; keep interiors and text in terminal theme colors. Preserve source while streaming and when a preview cannot fit or uses unsupported syntax.

## Release build fix for 0.7.6

- Restore Windows CLI builds by declaring the shared timestamp and file-lock dependencies for every platform.

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
