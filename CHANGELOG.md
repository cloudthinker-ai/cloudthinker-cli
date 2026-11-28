## [0.13.6]

- Ask for approval in the terminal with `agent -p` when you redirect the answer, and show who can approve when you need someone else.
- Includes the changes prepared for 0.13.5.
- Refresh the surface API bindings and patch the CLI's URL and QUIC dependencies.
- Show a short hint when the connection type is invalid, even after a long paste.
- The CLI shows the status saved by Connection Test.
- Keep Cyber app listing compatible with the current backend and refresh the Review response schema.
- Blank `agent -p` prompts and `cloud exec` commands now tell you what to enter.
- `cloud exec` tells you when a script file is too large, even if it contains Unicode text.
- Refresh the saved API schema for the new Connection status fields.

## [0.13.5]

- Run `cloudthinker update` even when GitHub limits update checks from a shared network.

## [0.13.4]

- The CLI uses the same workspace permissions as web Chat when it reads from your Cloudflare Connection.
- Approve or decline chat actions in the terminal and keep waiting for the answer.
- Use your saved workspace login to start and check chat runs.

## [0.13.3]

The CLI prints the session id and how to return when the agent screen closes.

- Copy the session ID and return command when you leave a local or remote agent.
- `cloudthinker login --token` shows the token's workspace role (Viewer, Developer or Admin) instead of a list of scopes.
- Public API command help and `cloudthinker api` name the workspace permission each operation needs, such as `incidents:edit`, instead of a token scope.
- `GET /v1/token` still returns `scopes` for older clients. The field is deprecated and now lists the same values as `permissions`: what the token can use after the owner's current role applies.
- Add `cloudthinker approvals list` to see the pending approval requests you can answer, with `--kind`, `--limit`, `--cursor` and `--json`.
- Add `cloudthinker approvals watch <request-id>` to follow one request until a person approves (exit 0), declines, or it expires or is superseded (exit 1). `--timeout` exits 4, Ctrl-C exits 130 and leaves the request open, and `--json` prints one line per state.
- `cloudthinker approvals requests list|get` from the public API keep working beside the new commands.
- `cloudthinker api --check` no longer reports every module that takes public API resources as a collision.
- `cloudthinker chat -p` now refuses a prompt of only whitespace instead of sending an empty run.
- When the server's API description is newer than this CLI, the error now says to run `cloudthinker update`.
- A blank `CLOUDTHINKER_WORKSPACE` or `CLOUDTHINKER_URL` (what a CI template leaves when a variable is unset) now counts as not set; a blank `--workspace` or `--url` flag says it needs a value.
- `cloudthinker login --token` and every command that signs in with `CLOUDTHINKER_TOKEN` also work on a server that still reports token scopes instead of a workspace role; `login --token` then prints `Scopes:` where it would print `Role:`.
- The CLI reads the public API description from servers that name an operation's permission `x-scope` as well as `x-permission`, so one CLI works on every CloudThinker server.
- Piped input over the 50,000-character limit now says the prompt is too long instead of printing a character count that was only a partial read.
- Chat rejects piped input that exceeds the read limit instead of sending an incomplete prompt when the cutoff falls in whitespace.
- Use `--all` to read many small API pages faster.
- `cloudthinker doctor` tells you to sign in again when API commands reject your saved login.
- Restore the check that builds the Linux Worker tests.
- Help, version, and the bundled guide open faster.
- Public API help and guides show valid status filters and how to pass IDs and ETags.
- Enter a prompt before continuing a chat. The CLI checks the prompt before looking up the conversation.
- Pass a workspace ID or leave out `--workspace`. Named API commands reject blank workspace values.
- Lists fetched with `--all` stop at 10,000 items or fewer. Use the returned cursor to fetch the next page.
- Restore the CLI formatting check before release.

## [0.13.2]

- This is the first stable release since 0.12.2. It also ships every change listed under 0.13.1 and 0.13.0, which were never published as stable releases. The main additions: a command for every CloudThinker public API operation and `cloudthinker api`, `cloudthinker login --token` for CI and scripts, `cloudthinker doctor` checks with a fix for each problem, and `cloudthinker review` findings shared with the pull request review.
- Refresh generated API models: Apps can be listed by archived state and built-in connections by prefix.

## [0.13.1]

- `cloudthinker agent --help` lists `--add-dir` and `--max-turns`.
- `cloudthinker doctor` now also checks the agent, its skills, the newest release on your update channel, terminal support, AGENTS.md or CLAUDE.md in this folder, and where CloudThinker is installed, with the steps to remove it.
- `doctor`, `worker doctor`, `cyber doctor` and `review doctor` print one line when all is well, and otherwise one row per problem with its fix.
- This release also ships every change listed under 0.13.0, which was never published as a stable release.

## [0.13.0]

- Add commands for every CloudThinker public API operation, read from the public API spec: `cloudthinker <module> <resource> <verb>` and `cloudthinker api`.
- Read `cloudthinker --skill api` for how an AI agent calls the public API from the CLI.
- Sign in for CI and scripts with `cloudthinker login --token < token.txt`, which reads a CloudThinker access token from stdin, checks it with the server, and saves it for each workspace it covers.
- `cloudthinker review findings <finding-id>` reads one finding; a merge-request URL still lists its findings.
- `incident ls`, `incident status`, `chat -p`, `chat status` and `chat ls` call the public API; flags and output are unchanged.
- An operation the server marks for confirmation, such as a delete, asks `[y/N]` on a terminal. A global `--yes` (`-y`) skips the question, and without a terminal the command refuses unless `--yes` is given.
- Public API commands take the workspace from a `CLOUDTHINKER_TOKEN` that covers one workspace.
- A mistyped command shows its usage error and a suggestion even when the server is unreachable.
- `cloudthinker review` no longer fails when the model wraps its JSON answer in prose or a code fence. It reads the findings object from the answer, asks the agent once more when no object is found, and drops single findings that do not point at a changed line or are incomplete instead of failing the run. It prints `Dropped N finding(s)…` on stderr, and `--json` adds `dropped_findings`.
- A diff over 40 KiB is reviewed in parts of up to 40 KiB, split by file, and the findings are merged. A single file whose diff is longer than 40 KiB is cut short, and the agent is told to read the whole file. `--timeout` applies to each part.
- The review runs on the workspace's Code Review model and uses the repository's review instructions, Learnings and Convention Rules from CloudThinker when the `origin` remote belongs to a connected repository. The agent also reads the nearest `AGENTS.md` and `CLAUDE.md`. When CloudThinker cannot supply them, the review warns once and uses the built-in instructions.
- After a review, the findings are sent to CloudThinker so the first review of the pull request for this branch credits them: it skips the ones you fixed and checks the ones still open. The CLI prints `Sent to CloudThinker: the PR review will credit these findings.`, and `--json` adds `uploaded`. `--no-upload` keeps the findings on your machine. A failed upload prints a warning and never fails the review.
- `cloudthinker --skill review` now prints a Claude Code skill: save it with `mkdir -p .claude/skills/cloudthinker-review && cloudthinker --skill review > .claude/skills/cloudthinker-review/SKILL.md` and run `/cloudthinker-review` to review, confirm, and fix your changes before you open a pull request.
- Refresh the local Cyber discovery bundle to match the current scope and manifest rules.
- Keep the CLI API models aligned with Cyber investigation timing and team limits.
- Local pentests share cloud investigation guidance, checkpoint continuation, independent execution references and report readiness. New ordinary cloud runs use backend-issued coverage while retaining evidence validation.
- `cloudthinker cloud tool-search --session <ID> <QUERY...> [--limit N]` replaces `cloudthinker cloud load-tools`. Plain words rank the tools your Connections offer; exact tool names (SDK tools as `prefix.tool`) return their full schemas. `load-tools` is removed, and the backend's old load route now answers with an upgrade message.
- `review watch` now applies its overall timeout to each in-flight status request. A request that finishes after the deadline returns the timeout outcome, and the in-flight request is dropped when the deadline expires.
- Reduce retained folder names during remote completion while preserving the existing first 50 results, lexical order and hidden-folder behavior.
- Guide local Pentest investigations through stack evidence, advisory research and independent verification.
- Refresh generated API models for the current backend worker and repository contracts.
- `cloudthinker update` on a remote machine keeps a running `cloudthinker remote` agent; the next connect uses the new version. Starting an agent on a machine that stops user processes at logout now says to run `loginctl enable-linger`.
- Run the CLI and the agent on Alpine and on older Linux servers such as RHEL and Rocky 8, Amazon Linux 2, Debian 11, and Ubuntu 20.04: releases add static musl builds for x86_64 and arm64, the installer and `update` pick them where glibc is older than 2.35 or absent, and the agent picks its musl build on Alpine.
- Clarify Cyber investigation ownership: each persistent Fortress teammate coordinates independent verification and saves supported findings without Manager approval. Bounded helpers return evidence, and the Manager owns the final Run report.
- Align generated Cyber API models with active finding counts, saved Run activity and current team limits.
- When a `ct_ask` run stops for approval, the terminal shows the step and lets an approver approve, decline with a reason, or decide in the browser; `ct_run_status` offers the same for a paused run. Forms, break-glass overrides and sub-agent approvals stay in the browser.
- Mirrored session entries are redacted on the server before they are stored, so credentials in a transcript stay out of the cloud copy; emails, IPs and hostnames are kept.
- An oversized mirrored entry keeps its structure and cuts only its longest text, instead of losing the message.
- Each background task writes its full output to its own log file, shown in the task's output and status.
- `CLOUDTHINKER_CONFIG_DIR` moves the CLI's credentials and worker state, so people who share one Unix account can each keep their own login; `whoami` prints the credentials path when it is set.
- Piped stdin that stays open now prints one stderr notice with the bytes received and the advice to pipe a finite snapshot; input past the limit stops the read at once.
- Device login shows when the code expires, with a live countdown on a terminal, and over SSH a plain `BROWSER` no longer overrides the device code; a VS Code forwarding helper still does.
- `chat trace` always states that tool arguments, tool output and thinking are excluded, says when older messages are capped, and points to the full record.
- The README now leads with the generally available workflows and moves pentest to a gated section.
- `CLOUDTHINKER_RELEASE_URL` installs the agent from an https mirror, a `file://` URL or a local directory laid out as `<base>/v<version>/<asset>`, with the same checksum checks; errors name the source and the variable.
- A bare `cloudthinker` over SSH runs the agent so it keeps running when the connection drops; run `cloudthinker` again to return to it, or set `CLOUDTHINKER_REMOTE=0` to run it in the shell.
- The cloud write approval fits the terminal height and scrolls the whole script with PgUp/PgDn and Home/End; Enter reads to the end of an unread script before it confirms.
- The running-command view fills 80% of the terminal, follows the tail, wraps long lines, scrolls, and searches with `/` and `n`/`N`.
- Ctrl+O transcript search finds matches past the 400-line cap of a tool result and opens that part of the result.
- `cloudthinker remote` accepts `user@host:port` and `host:port`, including `[::1]:2222`, and opens one SSH login that the attach and `cloudthinker update` share.
- After an SSH login fails, the reconnect waits 2, then 5, then 10 minutes instead of retrying every 30 seconds, so a server does not lock the account.
- Output from `~/.bashrc`, `~/.profile` or a login message on the remote machine no longer breaks the connection; more than 64 KiB of it stops with a message that names those files.
- `cloudthinker remote ls` lists the saved machines, `cloudthinker remote ls <machine>` lists the agents running there (`--json` for scripts), and `cloudthinker remote forget <machine>` removes one.
- `cloudthinker remote <machine> --detach` starts the agent without attaching and prints how to return to it; it works without a terminal.
- `cloudthinker remote <machine> --via '<command>'` reaches the machine with another command, such as `ssh -J`, and remembers it for later connects.
- Idle agent sessions wake about 20 times a second instead of about 150, and run 8 threads instead of 15: `cloudthinker agent` starts the agent with one garbage-collector helper thread (set `BUN_JSC_numberOfGCMarkers` to choose another count).
- On Alpine with `gcompat`, `cloudthinker agent` now installs the musl agent. It used to pick the glibc build, which failed with "symbol not found".
- When a new agent fails its start-up check, the error now shows the agent's last error line and a fix where one is known: a missing `libstdc++`, a C library mismatch, or a CPU without the needed instructions. It used to say only "exited with status".
- Hosts without `lsof` (minimal Alpine, Debian and Amazon Linux images) now remove old agent versions after an update. Each update used to leave another 120 MB on disk.
- The start-up logo now uses 256 colours when the terminal does not report 24-bit colour, as over SSH or in tmux. On the Linux text console (`TERM=linux`), it draws the logo in ASCII and skips the intro animation.
- In Manual approval mode, the agent now asks before a local command that may change something, a PowerShell command, or any file edit or write, in the same approval card as a cloud write. Read-only commands such as `ls`, `cat`, `rg`, `git status`, `kubectl get` and `aws describe`/`list`/`get` run without asking. Under `agent -p`, Manual blocks those calls and tells the agent why. Auto mode is unchanged.
- `cloudthinker login` with a device code now opens the approval page with the code filled in. Check that the page shows the code from your terminal, then continue; the bare URL is still printed for manual entry.
- `cloudthinker remote <host>` can now install the CLI on a Linux server that cannot reach GitHub. When cloudthinker is missing there, or too old and its own update fails, the laptop asks, downloads the matching Linux build and agent, checks their checksums, and copies them over the same SSH login. The agent on that server then installs from the copied files. The server needs only `tar`.
- `cloudthinker update --version 0.12.2` installs that release, older or newer, so a team can pin every host to one version or roll back a bad update. A version that was never released exits 2 and names the releases page.
- Background task cleanup reads each task manifest once per sweep instead of up to three times.
- Container cleanup removes all labelled session networks with one `docker network rm` call instead of one call per network.
- Internal: the CLI builds again from the current API, and its skill export test matches the Cyber guides it prints. No command changes.

## [0.12.2]

- `worker start` no longer fails its folder check when `output/` already holds files. The worker notes the files in `output/` when it starts and uploads only files that are new or changed after that. A folder with more than 64 files or 16 MB in `output/` used to fail with `EXECUTOR_ARTIFACT_LIMIT_EXCEEDED`. The failure line now says that `output/` is over the upload limit.

## [0.12.1]

- Internal: agent install and tool install share one size-capped download path. Timeouts, size limits and error messages are unchanged.
- Also ships the 0.12.0 notes, which were never published as a release.

## [0.12.0]

- Keep the CLI compatible with Doomsday apps and runs. The generated API accepts the new mode, and the bundled Cyber discovery runtime matches the backend manifest contract.
- Local Cyber run launch requests now match the current backend schema.
- Keep the CLI compatible with Fortress Runs. The generated API accepts the persisted mode value, and the bundled Cyber discovery runtime matches the backend manifest contract.
- Use Environments in help and setup guidance while retaining worker commands and outpost flags for compatibility.
- Search prior conversations in the selected workspace with `cloudthinker chat search "<text>"`. The query is literal text by default; `--regex` reads it as a regular expression, and `--fuzzy` finds near matches such as typos above a `--min-score` cutoff (default 85 of 100). Matching ignores case and reads only titles, message text, tool names, tool inputs, tool outputs and attachment names. Each match shows the title, the full conversation UUID, a redacted excerpt and a web link. Narrow it with `--since`, `--until`, `--time-field` and repeatable `--type`, cap it with `--limit` (1 to 50, default 10), and print one JSON result with `--json`. Every result says whether the search was complete, stopped at the limit, or partial. A partial search prints a `--cursor` to continue when one is available, and a partial search with no matches exits 1, because it does not prove absence. The command starts no agent run or model call, and it does not search thinking, hidden prompts or attachment contents.

## [0.11.2]

- On a Worker, the agent's file read now starts at the first line. It used to skip line 1, so a one-line file read back empty and a paged read started one line late.
- `worker start --register` now accepts a registration reference that starts with `-`. About 1 in 64 references did, and the setup command then failed with an argument error.
- On rootless Docker, a Worker set to container isolation can now write the served folder and read its egress allowlist. It used to run the container as a user that rootless Docker maps to a different account, so commands failed with `Permission denied`.
- On container isolation, files and folders that a command creates in the served folder are no longer writable by every user on the machine. Commands now run with umask 022, like a normal login shell; `docker exec` used to start them with umask 000.
- On a Worker that sandboxes commands (Loupe), a tool that your PATH reaches through a symlink now runs. A `pnpm`, `go` or `aws` linked into `~/.local/bin` from another folder used to fail with "permission denied" or "command not found", because the sandbox could read the link but not the folder it points to.
- `cloudthinker doctor` prints one aligned row per check with a ✓, !, ✗, ? or - mark. It names the signed-in account and workspace instead of their IDs, folds healthy Connections into one count, and lists only the Connections that need attention, by name, with a single fix. The report ends with one verdict line.
- `doctor` now shows where the server address came from (`--url`, `CLOUDTHINKER_URL`, the default saved by `login --url`, or the built-in default). It warns when `CLOUDTHINKER_URL` points away from your saved default, which used to make doctor check a different server than the one you signed in to. `doctor --json` adds `origin_source` and each Connection's `connection_name`. Its `summary` strings are shorter, and the account row now names the signed-in email instead of the user ID; `user_id` and `workspace_id` stay in their own fields.
- `login --url` warns when `CLOUDTHINKER_URL` is set to another server, because that variable overrides the new default in the same shell.

## [0.11.1]

- The resume line printed when the agent exits now reads `cloudthinker agent --session <id>`, a command that works. It used to print `cloudthinker --session <id>`, which the CLI rejected.

## [0.11.0]

- Cyber finding statuses are now called confirmed and fixed. Their folders stay `findings/open/` and `findings/resolved/` in this release; CloudThinker also reads `findings/confirmed/` and `findings/fixed/`.
- `cyber finding list --status open|resolved` keeps sending the old words `open` and `resolved` in this release, so an older CloudThinker server still understands it. A new server reads them as confirmed and fixed.
- Run history reads the new `findings_fixed` count and falls back to `findings_resolved` from an older server.
- An admin can set a Worker to container isolation. Each machine of that Worker then runs every shell and background command of a conversation in a session container; file reads and edits still act on the served folder, which the container mounts at the same path.
- The container runs as your account with no Linux capabilities, a read-only root, a private `/tmp` and home, and limits of 512 processes, 4 GB memory, and 2 CPUs. It has no DNS and no direct network. Its only way out is an egress proxy that allows HTTP and HTTPS to the hosts on the Worker's allowlist (exact names or `*.domain`); an empty allowlist blocks all outbound access. The proxy refuses IP addresses, private and metadata addresses, and names that do not resolve, and answers with a message such as `Blocked by the Worker egress policy: connect by host name, not by IP address.` The session network has no address on the host, so services on the machine are out of reach too. Allowlist changes reach a running conversation within a minute.
- Container isolation needs Docker on the machine. Podman is not supported yet. A machine without a working Docker cannot serve such a Worker: `worker start` prints the server message and exits with code 2, and a run fails closed with `EXECUTOR_ISOLATION_UNAVAILABLE` instead of running on the host. `worker doctor` reports the runtime, the daemon, and the images in a new "Container isolation" row.
- The Worker removes its containers after 15 minutes without commands, when it stops, and at the next start.
- Each in-flight run now authenticates with its own lease instead of the Worker credential.
- Start or resume a shared Worker's setup conversation with `cloudthinker review setup --worker <name-or-id>`. Open the printed web link to follow the agent and answer questions.
- Check Worker prerequisites with `cloudthinker review doctor --worker <id>`. Add `--connection <id> --provider <provider> --repository <path>` to see stored setup observations; doctor does not rerun the browser journey.
- `cloudthinker chat trace <conversation-id>` shows persisted messages, tool metadata, recorded message failures, tokens and credits in text or `--json`. It reads an authorized workspace snapshot without starting work, preserves unknown usage, and reports truncation. Tool arguments, outputs and thinking are excluded; mirrored Agent CLI sessions are unsupported.
- `cloudthinker agents ls` and `agents show <uuid-or-alias>` discover workspace agents in text or JSON. They require authentication, distinguish default and inactive agents from custom agents eligible for `chat --agent`, reject ambiguous aliases, and show bounded metadata without prompts, souls, or configuration.
- Add `cloudthinker doctor [--json]` to diagnose CLI credentials, live workspace identity, API access, and recorded Connection test status with bounded waits. It reports missing evidence and safe recovery actions without starting login, repairs, or provider tests.
- Export workspace Policy as YAML with `cloudthinker policy export`, or save a private file with `--output`. Preview a YAML or JSON import with `cloudthinker policy import <path>` and write only with `--apply`. Optional pruning and Allow Rule activation are explicit; document issues return a nonzero exit and remain available in one `--json` report.
- `cloudthinker review setup` no longer sends its own Loupe setup prompt. The server starts the setup run on the selected Worker, and the command prints the conversation link.
- `cloudthinker worker start` without `--label` names the machine `<folder> · <hostname>` instead of leaving it unnamed.
- Add `cloudthinker workspace ls [--json]` to list, live, the workspaces you can access in your current login's organization. It marks the selected workspace, shows your role, and says whether this machine holds a saved login for each one. Listing never changes the active login; a workspace in another organization needs `cloudthinker login`.
- New `cloudthinker connections disconnect <target>` removes a built-in or MCP Connection and its saved credentials from your workspace (workspace admins). It asks first and names the Connection; `--yes` skips the question, and without a terminal the command needs `--yes`. The disconnect finishes in the background, and `--json` prints the Connection with its `disconnect_phase`.
- A Worker now delivers a finished change after a network or server outage, even a long one, instead of leaving it reported as unknown. It keeps the result on disk and sends it again when it reconnects or restarts, and a run whose access ran out during the outage no longer stops the Worker.
- A Worker whose saved result keeps failing to send now logs it, keeps it on disk, and still takes new work.
- Journals written by this version cannot be read by an older CLI. Downgrading the Worker after this version is unsupported.
- A Worker keeps a receipt journal that holds an unsent result past the 7-day cleanup, up to 90 days, and sends saved results before it cleans up old journals. It drops a saved result CloudThinker no longer knows or that belongs to another operation, instead of trying again on every reconnect.
- A receipt journal the Worker cannot open no longer stops it from starting; it warns with the file path and skips that journal.
- After it reconnects, a Worker spends at most one minute sending saved results before it takes new work, and tries the rest once a minute while it has no run. An operation waiting to send its result no longer holds one of the 16 run slots.

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
