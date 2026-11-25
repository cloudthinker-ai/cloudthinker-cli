# CloudThinker CLI

[![Latest release](https://img.shields.io/github/v/release/cloudthinker-ai/cloudthinker-cli?label=release)](https://github.com/cloudthinker-ai/cloudthinker-cli/releases/latest)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Platforms: macOS | Linux](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-lightgrey.svg)

`cloudthinker` brings [CloudThinker](https://cloudthinker.io) to your terminal.
Type `cloudthinker`, describe what you want in plain language, and watch the
agent work in your current directory. For example, ask it to review your
changes, investigate a failing deployment, or explain a cloud bill; CloudThinker
checks and repairs its local setup, runs the work, and shows progress and
results in the terminal. Headless chat and focused commands remain available
for scripts and automation.

```sh
curl -fsSL https://cloudthinker.io/install.sh | sh
cloudthinker login
cloudthinker
```

## What you can do

| Task | Command |
| --- | --- |
| Start work in the current directory | `cloudthinker`, then describe the task |
| Investigate a cloud or deployment problem | `cloudthinker`, then say “Why is the checkout service returning 500s?” |
| Ask CloudThinker one question and pipe the answer | `cloudthinker chat -p "Check production health"` |
| Review local Git changes with CloudThinker | `cloudthinker review [--base <REF>]` |
| Follow a CloudThinker review on a merge request or pull request | `cloudthinker review watch <MR_URL>` |
| Let CloudThinker conversations work in a local folder | `cloudthinker worker start --outpost <name> --workdir "$PWD"` |
| Diagnose CLI login, workspace, API access, and Connection status | `cloudthinker doctor` |
| Export workspace Policy or preview a Policy file | `cloudthinker policy export`, `cloudthinker policy import policy.yaml` |
| Give an AI agent the CLI's internal operating guide | `cloudthinker --skill` |

## Install

```sh
curl -fsSL https://cloudthinker.io/install.sh | sh
```

The installer supports macOS (Apple Silicon and Intel) and Linux (x86_64 and
arm64). It puts the `cloudthinker` binary in `~/.local/bin` and adds that
directory to your `PATH`. Open a new shell after the install.

On Linux, the installer picks the build for your system. Systems with glibc
2.35 or newer get the glibc build. Older systems, such as RHEL and Rocky 8,
Amazon Linux 2, Debian 11, and Ubuntu 20.04, get the static musl build, and so
does Alpine. `cloudthinker update` picks the same way. On Alpine, the agent
also needs the C++ runtime: `apk add libstdc++`.

In a terminal, the installer asks once whether to use `ct` as a shortcut for
`cloudthinker`, before opening login. Press Enter to keep `cloudthinker`.
Choose Yes to add a persistent alias for Bash, Zsh, or Fish, then open a new
terminal. Both names work. Existing `ct` commands, aliases, and functions in
your shell settings stay unchanged. Updates and automated installs skip the prompt.
Other shells show a hint for setting up the shortcut yourself.

Every release is on the [releases page](https://github.com/cloudthinker-ai/cloudthinker-cli/releases),
with a SHA-256 checksum for each archive.

Run `cloudthinker update` to update. On an interactive terminal, the CLI also
offers the update when a newer release exists, and other commands end with one
line that names it. It checks at most every 20 hours.
Answer `s` to skip that version until a newer one ships. Set
`CLOUDTHINKER_NO_UPDATE_CHECK=1` to turn off the offer.

Run `cloudthinker update --version 0.12.2` to install one release from the
releases page. An older version rolls back. To stay on it, skip the offer
with `s` or set `CLOUDTHINKER_NO_UPDATE_CHECK=1`.

## Quick start

```sh
cloudthinker
```

On first use, CloudThinker verifies your login and opens the interactive agent.
Describe the task there, for example “Review my staged changes” or “Why did
last night's deploy fail?”. The agent works in your directory and asks before
it changes anything that matters.

```sh
cloudthinker review                                      # review your local Git changes
cloudthinker chat -p "Check production health"           # one question, answer on stdout
cloudthinker review watch <MR_URL>                       # follow a review on a merge request
```

Pentesting is gated. It is available to workspaces where it is enabled; ask your
CloudThinker contact to turn it on. See [Pentest](#pentest-gated).

Over SSH, or on Linux with no display, `login` shows a short code to enter in
any browser instead of waiting for a loopback callback. `login --device-auth`
forces the code, and `CLOUDTHINKER_LOGIN=browser` forces the browser callback.
`login --no-browser` prints the consent URL without opening a browser.

`login --url <origin>` remembers that address, so later commands need no
`--url`. Log in with `--url https://app.cloudthinker.io` to return to
production. A logout that removes the last login for the remembered address
also returns the default to production.

Shell completion:

```sh
cloudthinker completion bash > ~/.local/share/bash-completion/completions/cloudthinker
cloudthinker completion zsh > "${fpath[1]}/_cloudthinker"
cloudthinker completion fish > ~/.config/fish/completions/cloudthinker.fish
```


## Pentest (gated)

Local pentesting runs only in workspaces where CloudThinker has enabled it. Ask
your CloudThinker contact for access. Once enabled, run `cloudthinker`, then say
“Pentest http://localhost:3000”. The agent checks its Cyber setup, repairs its
pinned tools, saves confirmed project settings in `.cloudthinker/config.toml`,
runs the pentest, and reports progress and results. It asks only when required
target or authorization details cannot be determined safely.

## Diagnose CLI setup

```sh
cloudthinker doctor
cloudthinker doctor --json
cloudthinker --url https://dev.cloudthinker.io --workspace '<workspace-id>' doctor
```

Doctor reports the installed CLI version, the server and where its address came
from, the selected credential source, public API reachability, the signed-in
account and workspace, and recorded Connection test status. It prints one row
per check, lists only the Connections that need attention, and ends with an
overall verdict and one action for each problem. It warns when
`CLOUDTHINKER_URL` points away from the default that `login --url` saved. JSON
mode writes one object on stdout, including failed checks and `exit_code`.

Doctor reads status only. It starts no provider test, retry, repair, browser,
login, agent, or model. A stored login may refresh through the normal guarded
flow, which saves rotated credentials without changing the active workspace.
`CLOUDTHINKER_TOKEN` stays read-only and cannot combine with `--workspace`.

Doctor does not inspect or migrate legacy keyring credentials. If no compatible
file credential is selected, that is missing evidence, not proof that an older
login expired. Run `cloudthinker whoami` first to inspect or migrate that login.
Use the host-aware command doctor prints, then sign in only if needed.

Each check has one of these statuses:

| Status | Meaning |
| --- | --- |
| `healthy` | The stated observation succeeded; read its evidence limit |
| `warn` | An observed condition needs attention, such as pending tests or no installed Connections |
| `error` | A check or recorded Connection test failed |
| `skipped` | A prerequisite was unavailable, or an optional check was not attempted |
| `unknown` | Available evidence cannot establish the result |

The JSON object contains `cli_version`, `origin`, `origin_source` (`flag`,
`environment`, `saved`, or `built_in`), `overall`, `user_id`,
`workspace_id`, `checks`, `connections`, and `exit_code`. Each check includes its
`id`, `status`, `summary`, optional `action` and `cause`, optional `connection_id`,
`connection_name`, and `kind`, optional `connection_status` and `test_phase`, and optional
`http_status`. New server status or phase values project to `unknown`, without
echoing their raw contents. Unavailable values are JSON `null`.
The Connection counters report `installed`, `checked` snapshots, and `omitted`
snapshots without a result. An unavailable inventory has null counts, not zero.

Requests time out after 3 seconds. Credential locks wait up to 1 second.
Connection reads share 15 seconds, with at most 4 requests active and 20 IDs
selected in sorted order. Network work has a 70-second budget; a stuck local
filesystem is outside that bound. Missing snapshots remain unknown.

A successful snapshot may come from persisted Connection status. It does not
prove a fresh provider request succeeded. The public health probe only proves
that endpoint answered. If whoami succeeds while the public probe is unavailable,
doctor reports the probe limitation as a warning. Doctor checks neither release
freshness nor server version compatibility and never starts an update check.

Output omits tokens, URL userinfo/query/path, private configuration, arbitrary
labels, and raw provider errors. Use the reported Connection IDs to inspect
individual rows in the app.

| Exit | Meaning |
| ---: | --- |
| 0 | Checks succeeded, possibly with warnings |
| 1 | Failed or incomplete evidence, including unknown phases, omitted snapshots, or transport errors |
| 2 | Invalid configuration, credential selection, or rejected request |
| 3 | Authentication unavailable/rejected, or permission denied |
| 4 | An explicit diagnostic deadline elapsed |
| 130 | Connection reads were interrupted |

With several causes, selection/request errors take priority, followed by auth,
deadlines, and other failures. An interrupt exits 130. A recorded server test
timeout exits 1; a native transport timeout retains exit 1. Neither is an expired
login. The `review doctor`, `cyber doctor`, and `worker doctor` commands retain
their existing behavior.

## CloudThinker Agent

`cloudthinker` opens the interactive CloudThinker Agent in your directory. The
agent can edit files, run shell commands, and operate supported CloudThinker
workflows such as local Cyber pentests. It configures and repairs workflow
settings itself when the required facts are available. The model and your
workspace Connections stay in the cloud.

```sh
cloudthinker agent
cloudthinker agent -p "Add a health check to main.tf" --model cloudthinker/pro
cloudthinker --workspace Production agent          # the CLI's own options come first
cloudthinker agent -- --url http://localhost:3000  # after `--`, every argument goes to the agent
```

When `agent -p` shows an approval in your terminal, review the action and
press `y` to approve or `n` to decline. Press Enter and follow the link to
decide in your browser.

The agent receives every argument after `agent` without change. On the first
run, the CLI downloads the agent build for your platform from the same GitHub
release as the binary. On Linux, that is the musl build when the system has no
glibc, as on Alpine, and the glibc build otherwise. It checks the build against the release's SHA-256
sidecar and installs it under `~/.cloudthinker/agent/bin/<version>/`. Later runs
reuse that build, and a new version replaces it.

The interactive agent is the one command that starts a login by itself. Focused
commands continue to print a login hint when authentication is missing.

The first empty boot opens with the centered Cutout cloud artwork. After your
first message, the small Cloud outline keeps the session details visible.
`/new`, resume, reload and session switches use the compact header immediately;
they do not replay the large banner.

The artwork adapts to the terminal and the space needed by startup warnings.
Short terminals or long account details use the compact header. Ctrl+O before
the first message expands startup details and key hints; after a message it
opens the transcript. `/cloudthinker:about` shows versions and attribution. `/skill:tour` starts the guided tour. Startup
warnings stay visible even when the large artwork is shown. Ordinary startup
resource summaries yield to the artwork and remain available in the expanded
details. Saved quiet-startup preferences remain supported.

The first launch after you install or update plays a short intro: the cloud
gathers from scattered dots in about 1.4 seconds, then the prompt and status
line appear. You can type at once, and any key skips the intro. Later launches show the still cloud. To turn the intro
off, open `/settings` and set **Intro animation** to `false`.

`CLOUDTHINKER_CLI_MOTION=1 cloudthinker agent` plays the intro on every launch
and adds a brief highlight on the session outline. `CLOUDTHINKER_CLI_MOTION=0`,
`NO_COLOR`, `TERM=dumb`, and redirected output show no motion.

Over SSH, Mosh, and `cloudthinker remote`, the CLI redraws less to save
bandwidth: the intro draws every fourth frame, and spinners tick every 400 ms
instead of 80 ms. `CLOUDTHINKER_CLI_MOTION=1` restores full motion.

## Paste a screenshot over SSH

Ctrl+V in the agent reads the clipboard of the machine the agent runs on. Over
plain SSH that is the server, so in kitty the agent asks the terminal for the
clipboard image (kitty asks once to allow it) and pastes the saved PNG's path.
In iTerm2, Terminal.app, WezTerm, Windows Terminal, and inside tmux there is no
route over a plain SSH session: Ctrl+V says so and names the next step, `scp`
the image and paste its path, or use `cloudthinker remote <host>`, where Ctrl+V
reads your laptop clipboard.

## Discover workspace agents

Find an agent's UUID or alias before selecting a custom agent for a chat turn:

```sh
cloudthinker agents ls
cloudthinker agents show <uuid-or-alias>
cloudthinker agents ls --json
cloudthinker agents show <uuid-or-alias> --json
cloudthinker chat -p "Review the rollout plan" --agent <custom-agent-uuid>
```

These reads require login and permission to view agents in the selected workspace.
Use the global `--workspace <id-or-name>` when needed. They start no agent or model call.
Aliases match exactly, ignoring ASCII case. Duplicate aliases require a UUID.

`CHAT` is `eligible` for active custom agents, `inactive` for inactive custom agents,
and `default` for the primary default agent. Use chat without `--agent` for the default.
Chat checks its own permissions and runtime requirements when you submit a turn.
The roster shows the primary default agent and custom agents; it excludes other built-in specialists.

JSON list output is an array; show output is one object. Both use these fields:

| Field | Meaning |
| --- | --- |
| `id`, `alias`, `name` | Agent UUID, exact alias, and display name. |
| `agent_type`, `is_active` | `default` or `custom`, and current activity. |
| `description`, `description_truncated` | Agent purpose, capped at 2048 Unicode scalar values, and whether it was clipped. |
| `chat_selection` | `eligible`, `inactive`, or `default`; selection eligibility does not guarantee a successful chat. |

Output excludes prompts, instructions, souls, configuration, avatars, and tool payloads.
Human output removes terminal controls and bidi controls. JSON escapes control characters.
Names and aliases over 255 Unicode scalar values fail the read instead of hiding an identifier.
Lists sort by ASCII-case-insensitive alias, original alias, then UUID. An empty list succeeds with `[]` in JSON.
The API has no pagination. More than 1000 rows or an inconsistent roster fails the read; use Agents in the browser.
Missing, deleted, and foreign-workspace IDs return 404 and exit 1. Invalid or ambiguous references exit 2.
Authentication and permission failures keep exit 3. These commands do not open a login or fall back to public defaults.

## Headless chat

`chat -p` sends a prompt to CloudThinker and waits for the answer. stdout carries only
the final answer, so you can pipe it. Progress and continuation hints go to
stderr.

```sh
cloudthinker chat -p "Check production health"
cloudthinker chat -p "Draft the rollout plan" --json
kubectl logs deploy/api --tail 200 | cloudthinker chat -p "Why does this crash?"
cloudthinker chat -p - < prompt.md
```

Piped stdin joins the prompt. `-p -` reads the whole prompt from stdin. Piped
input is a finite batch: the CLI reads it to the end, up to 50,000 characters,
and then sends the prompt. It never streams. If the input has not ended after a
few seconds, the CLI says so on stderr; pipe a snapshot such as `tail -n 200 app.log`,
not `tail -f`. Input past the limit stops the read at once with an error. On a
terminal a spinner shows the run status while you wait. Ctrl-C stops the wait
and prints the command that resumes it; the run continues on the server.

If a run asks to take an action, review it and enter `y` to approve or `n` to
decline. Chat keeps waiting for the answer. Press Enter and follow the link to
decide in your browser. For browser-only requests, piped input, or `--json`, the command exits
with code 5. Open the run in your browser to decide.

Continue a thread with a run UUID or a conversation UUID. After each finished
run, the CLI prints `continue_with=<conversation_id>` on stderr:

```sh
cloudthinker chat -p "Remove the risky step" --continue <run-or-conversation-uuid>
```

Submit without a wait, collect the run later, or find an ID in recent runs:

```sh
cloudthinker chat -p "Audit production" --no-wait --json
cloudthinker chat status <run-uuid> --wait
cloudthinker chat ls --limit 10
cloudthinker chat ls --conversation <conversation-uuid> --json
cloudthinker chat trace <conversation-uuid>
cloudthinker chat trace <conversation-uuid> --limit 20 --json
```

`--no-wait` prints the IDs of the submitted run, because no answer exists yet.
`--timeout <secs>` limits only the client's wait. The run continues on the
server.

`chat trace` reads a bounded persisted snapshot of messages, tool metadata, recorded
message failures, tokens and credits. It starts no work. Unknown usage stays null;
tool completion does not establish success. Inspect `truncation` in JSON before
treating the returned window as complete. Tool arguments, outputs and thinking
are excluded. Mirrored Agent CLI sessions are unsupported. The source budget
covers the full conversation, so reducing `--limit` may not resolve overflow.

Search earlier conversations in the selected workspace:

```sh
cloudthinker chat search "lifecycle policy"
cloudthinker chat search OOMKilled --since 2026-10-01 --type rca --limit 20 --json
cloudthinker chat search --regex 'eks-[0-9]+'
cloudthinker chat search --fuzzy "kubernets upgrade" --min-score 80
cloudthinker chat search "lifecycle policy" --cursor <next_cursor>
cloudthinker chat search -- "--force"
```

`chat search` searches only conversation titles, message text, tool names,
tool inputs, tool outputs and attachment names. JSON keys, IDs, thinking,
hidden prompts and attachment contents are never searched. Matching ignores
case. Pick at most one mode:

| Mode | Matches |
|---|---|
| default | The exact text, punctuation included. |
| `--regex` | A Python regular expression. `^` and `$` match at each line in a field. An invalid pattern exits 2. |
| `--fuzzy` | Approximate text by string similarity, not by meaning. `--min-score` (60 to 100, default 85) sets the cutoff. |

The fuzzy score is the best similarity, from 0 to 100, between the query and
a same-length stretch of the field, after lowercasing. A field shorter than
the query is compared whole. Each match shows the title, the full
conversation UUID, the field, a redacted excerpt, the fuzzy score and a web
link. Search starts no agent run or model call.
The status is `complete`, `limit_reached` or `partial`. A partial page lists
its reasons and, when it can continue, a `next_cursor` to pass to `--cursor`
with the same query and filters. A partial search with no matches exits 1,
because it does not prove there are none.

## Code review

Review local changes through CloudThinker and print the findings in your terminal:

```sh
cloudthinker review                         # staged, unstaged, and untracked changes vs HEAD
cloudthinker review --base origin/develop   # branch diff from the merge base, including dirty edits
cloudthinker review --json                  # one machine-readable result on stdout
cloudthinker review --fail-on high          # exit 6 when a high or critical finding exists
cloudthinker review --no-upload             # keep the findings on this machine
```

The command requires `cloudthinker login`. It starts the bundled local Pi agent in the checkout with read-only `read`, `grep`, `find`, and `ls` tools. CloudThinker supplies model inference, so model requests and responses pass through its gateway and normal observability traces. The local Pi transcript and findings are not mirrored as Agent CLI session entries or saved chat messages; the session metadata uses a generic title and cwd. Findings are validated against changed paths and lines and printed here. It does not create a source-control review, post comments, apply fixes, or change the Git index or worktree. `--timeout <secs>` limits each local agent run; rerun the command after a timeout.

A diff over 40 KiB is reviewed in parts of up to 40 KiB, split by file. The review runs on the workspace's Code Review model. When the `origin` remote is a repository connected to CloudThinker, the review also uses that repository's review instructions, Learnings and Convention Rules, and sends the findings to CloudThinker so the first review of the pull request for this branch can credit them: fixed findings are not reported again, and open ones are checked. `--no-upload` keeps the findings on this machine. `--json` reports `uploaded` and `dropped_findings` (findings that did not point at a changed line).

Use it from Claude Code as a skill that reviews, confirms, and fixes your changes before you open a pull request:

```sh
mkdir -p .claude/skills/cloudthinker-review && cloudthinker --skill review > .claude/skills/cloudthinker-review/SKILL.md
```

Then run `/cloudthinker-review` in Claude Code.

Prepare Loupe on a shared Worker:

```sh
cloudthinker review setup --worker <WORKER_NAME_OR_ID>
cloudthinker review doctor --worker <WORKER_ID>
```

Setup starts or resumes a Worker-bound CloudThinker conversation. Follow its link to see the agent's work and answer questions. If the workspace has one shared Worker, you can omit `--worker`. Doctor checks Worker prerequisites; pass `--connection <ID> --provider <PROVIDER> --repository <PATH>` with `--worker` to also see the stored setup observations for that repository.

Inspect a CloudThinker review of a merge request or pull request by its URL:

```sh
cloudthinker review status <MR_URL>
cloudthinker review findings <MR_URL>          # worst severity first; a finding ID reads one
cloudthinker review watch <MR_URL> --json      # poll until the review finishes
cloudthinker review watch <MR_URL> --fail-on high
```

## Policy files

Export your workspace's Policy, edit the file, and preview its changes before applying:

```sh
cloudthinker policy export --output policy.yaml
cloudthinker policy import policy.yaml
cloudthinker policy import policy.yaml --apply --json
```

Export without `--output` prints only YAML. An output file must not exist;
the CLI publishes it privately after the download completes. Relative paths
use your current directory, and parent directories must already exist.

Import accepts UTF-8 YAML or JSON. It previews by default and writes only
with `--apply`. Export needs `agent_policy:view`; preview and apply both
need `agent_policy:edit` in the selected workspace.

`--prune` dismisses absent slugged Rules and clears omitted overrides when
the file includes `origin_levels`. `--activate` activates new or changed
Allow Rules; otherwise those Rules wait as proposals. Both flags default
to false. Preview either flag first, then repeat it explicitly with `--apply`.

The report lists mode, applied status, counts, Rule and section changes,
and document issues. `--json` prints one object, including validation failures.
Document errors exit 2 even when the server returns HTTP 200; forbidden
requests exit 3. Output, transport, conflict, and protocol failures exit 1.

Apply recomputes the plan under server locks against current state.
A preview does not freeze a revision. If apply loses its response, export
and preview again before applying another time. A report output error does
not undo a successful server apply.

## Outposts

An outpost lets a CloudThinker conversation work in a directory on your
machine. The worker connects outward over HTTPS and opens no inbound port.

```sh
cloudthinker worker outpost create my-project
cloudthinker worker start --outpost my-project --workdir "$PWD" --concurrency 4
```

File tools stay inside the selected directory. Shell commands run under your OS
account and are not sandboxed. To keep an outpost available after the terminal
closes, add `--background` to `worker start`, or run `cloudthinker worker
service install`. It writes a systemd user unit
on Linux or a LaunchAgent on macOS. `cloudthinker --skill worker` gives the full
guide.

## Use with coding agents

The binary carries an internal operating guide for AI agents that matches its
release. People start with `cloudthinker`; agents can read the guide when they
need to operate the CLI themselves:

```sh
cloudthinker --skill
cloudthinker --skill chat
```

The hub holds shared rules and routes an agent task to a module: `auth`, `chat`,
`review`, `cyber`, or `worker`. Each command prints Markdown and exits. It needs
no login, no network access, and no local agent. Users do not need to run these
commands or learn the Cyber command sequence. To make the skill available to a
coding agent, save the hub in that agent's skill directory:

```sh
mkdir -p .agents/skills/cloudthinker-cli
cloudthinker --skill > .agents/skills/cloudthinker-cli/SKILL.md
```

The source of the skill is `crates/cloudthinker-cli/skills/cloudthinker-cli/`.

## Workspaces and credentials

When your account can reach more than one workspace, login asks which one to
authorize. Each workspace credential stays available for the same host.
`--workspace` selects a credential by workspace ID or exact name:

```sh
cloudthinker --workspace Production whoami
cloudthinker --workspace 11111111-1111-4111-8111-111111111111 chat -p "..."
cloudthinker logout                 # the selected or active workspace only
cloudthinker logout --all           # every workspace for this host
```

For CI and scripts, sign in with an access token (`ct_pat_...`) from the
token page. Pipe it on stdin; the CLI refuses a token typed on screen:

```sh
cloudthinker login --token < token.txt
```

The CLI checks the token with the server, saves it for every workspace it
covers, and makes the first one active (`auth switch` picks another). It never
refreshes; make a new token when it expires.

List the workspaces you can reach, checked live, before you pick one:

```sh
cloudthinker workspace ls
cloudthinker workspace ls --json
```

The list covers only the organization of your current login. To reach a
workspace in another organization, run `cloudthinker login` and choose it in
the browser. `*` marks the selected workspace: `--workspace`, else the active
saved login. `SAVED LOGIN` says whether this machine holds a login for that
workspace, so `--workspace <ID>` or `auth switch <ID>` works with it. Listing
never changes the active login. JSON output is an array of objects with `id`,
`name`, `role`, `organization_name`, `is_default`, `selected`, and
`saved_login` (`null` when local logins cannot be read). An empty list succeeds
with `[]`. Authentication and permission failures exit 3. While
`CLOUDTHINKER_TOKEN` is set, commands use that token and reject `--workspace`,
so the command warns instead of suggesting it. The list is a point-in-time
snapshot: a workspace created or deleted while it is read can be missed, so
run it again before you conclude that a workspace is absent.

A tool that needs its own bearer can read one from the CLI. `auth token` prints
the current access token on stdout and nothing else. It refreshes the token
first when the token is close to expiry:

```sh
cloudthinker auth token
```

Keep that value secret. It authenticates as you until it expires.

The CLI stores credentials in `cloudthinker/credentials.json` under the
operating system's config directory, with mode 0600.

## Public API

Every CloudThinker public API operation is a command. The CLI reads the
operation list from the server's public API spec, so a new operation needs no
CLI update. Set `WORKSPACE_ID` and `REVIEW_ID` to the IDs you want to use.

```sh
cloudthinker api --list
cloudthinker resolve incidents list --status OPEN --all
cloudthinker review reviews get "$REVIEW_ID" -w "$WORKSPACE_ID"
cloudthinker chat runs list --status running
cloudthinker api GET "/v1/workspaces/$WORKSPACE_ID/resolve/incidents" --query status=OPEN
```

To resolve an incident, set `INCIDENT_ID` to its ID. Run the first command
and set `ETAG` to the value it prints, including the quotes if shown.

```sh
cloudthinker resolve incidents get "$INCIDENT_ID"
cloudthinker resolve incidents update "$INCIDENT_ID" --set status=RESOLVED --etag "$ETAG"
```

- Path IDs are positional; query parameters are flags. `--all` follows
  `next_cursor` (at most 10,000 items). Output is the response JSON.
- `-w <workspace-id>` picks the workspace; without it, the workspace of your
  login is used.
- Writes send an `Idempotency-Key` (pass `--key` to retry safely).
  A response `ETag` is printed on stderr; pass it back with `--etag`.
- A write runs when your token's role and your workspace role allow it. An operation the server
  marks for confirmation (`x-confirm` in the spec, every delete by default)
  asks `Run <METHOD> <path> (<operation>)? [y/N]` on a terminal; any answer
  but `y` or `yes` cancels it (exit 1). Pass `--yes` (`-y`) to skip the
  question. Without a terminal the command refuses unless `--yes` is given
  (exit 2).
- `cloudthinker api METHOD PATH` accepts only `/v1/` paths that match a public
  operation, and never follows a redirect.
- The spec is saved under `~/.cloudthinker/public-api/` and checked again after
  6 hours; when the server is unreachable the saved copy is used with a warning.
- `chat`, `review`, `cyber` and `workspace` hold both their own commands and
  the public API resources: `cloudthinker review --help` lists both.
- `cloudthinker review findings <MR_URL>` lists a merge request's findings;
  `cloudthinker review findings <finding-id>` reads one finding.

## Environment

| Variable | Effect |
| --- | --- |
| `CLOUDTHINKER_URL` | API base URL. Defaults to the origin of your last `login --url`, else `https://app.cloudthinker.io`. Pass the bare origin. |
| `CLOUDTHINKER_LOGIN` | `browser` or `device` overrides the login flow that `login` picks for this machine. |
| `CLOUDTHINKER_WORKSPACE` | Same as `--workspace`. |
| `CLOUDTHINKER_CONFIG_DIR` | Folder for this CLI's credentials and worker state. Defaults to the platform's `cloudthinker` config folder. Set it per person when several people share one Unix account. `whoami` prints the credentials path when it is set. |
| `CLOUDTHINKER_TOKEN` | Uses this bearer instead of stored credentials. Cannot be combined with `--workspace`. |
| `CLOUDTHINKER_NO_UPDATE_CHECK` | Turns off the start-up update offer and the update notice. |
| `CLOUDTHINKER_CLI_MOTION` | `0` turns off the launch intro. `1` plays it on every launch and adds a brief session-outline highlight. Applies only in an interactive color terminal. Default: the intro plays once after each install or update, with reduced motion over SSH, Mosh, and `cloudthinker remote`. |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | The job failed: a FAILED run, an unknown run, or exhausted transport retries. |
| 2 | Bad usage, or a server-side validation or secret-gate rejection. |
| 3 | Not logged in, the credential expired, or the server refused access (403). |
| 4 | A client deadline elapsed. The run continues server-side. |
| 5 | The run paused for human approval in the browser. |
| 6 | A review found an issue at or above its `--fail-on` severity. |
| 130 | Ctrl-C stopped a `chat` wait. The run continues server-side. |

## Build from source

The workspace needs Rust 1.90 or later.

```sh
cargo build --release -p cloudthinker-cli   # the binary is target/release/cloudthinker
make check                                  # fmt, clippy -D warnings, and tests
```

The `crates/` directory holds three crates:

- `cloudthinker-cli` is the binary, its commands, and the bundled skill.
- `cloudthinker-client` owns the HTTP client, login, and the token store.
- `cloudthinker-api` is generated from `openapi/cloudthinker-cli.json`. Do not edit it by hand.

`agent-cli/` holds the source of CloudThinker Agent that `cloudthinker agent`
downloads.

## Contributing

Report a bug or request a feature in
[GitHub Issues](https://github.com/cloudthinker-ai/cloudthinker-cli/issues).
This repository is a publish mirror of the CLI tree in CloudThinker's internal
monorepo. Each sync replaces the tree, so a maintainer applies an accepted pull
request upstream instead of merging it here.

## Documentation

Read the product documentation at <https://docs.cloudthinker.io>.
[CHANGELOG.md](CHANGELOG.md) lists the changes in each release.

## License

The CloudThinker CLI is licensed under the [Apache License, Version 2.0](LICENSE).

CloudThinker Agent (`cloudthinker agent`) is built on the
[pi](https://github.com/earendil-works/pi) agent harness by Mario Zechner, which
is licensed under the MIT License. [NOTICE](NOTICE) lists pi and the other
third-party components with their license texts.
