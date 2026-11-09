# CloudThinker CLI

[![Latest release](https://img.shields.io/github/v/release/cloudthinker-ai/cloudthinker-cli?label=release)](https://github.com/cloudthinker-ai/cloudthinker-cli/releases/latest)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Platforms: macOS | Linux](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-lightgrey.svg)

`cloudthinker` brings [CloudThinker](https://cloudthinker.io) to your terminal.
Type `cloudthinker`, describe what you want in plain language, and watch the
agent work in your current directory. For example, ask it to pentest an API;
CloudThinker checks and repairs its local setup, runs the work, and shows
progress and results in the terminal. Headless chat and focused commands remain
available for scripts and automation.

```sh
curl -fsSL https://cloudthinker.io/install.sh | sh
cloudthinker login
cloudthinker
```

## What you can do

| Task | Command |
| --- | --- |
| Start work in the current directory | `cloudthinker`, then describe the task |
| Ask for a local pentest | `cloudthinker`, then say “Pentest http://localhost:3000” |
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

## Quick start

```sh
cloudthinker
```

On first use, CloudThinker verifies your login and opens the interactive agent.
Describe the task there; for a local pentest, name the target in plain language.
The agent checks its Cyber setup, repairs its pinned tools, saves confirmed
project settings in `.cloudthinker/config.toml`, runs the pentest, and reports
progress and results. It asks only when required target or authorization
details cannot be determined safely.

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

The agent receives every argument after `agent` without change. On the first
run, the CLI downloads the agent build for your platform from the same GitHub
release as the binary. It checks the build against the release's SHA-256
sidecar and installs it under `~/.cloudthinker/agent/bin/<version>/`. Later runs
reuse that build, and a new version replaces it.

The interactive agent is the one command that starts a login by itself. Focused
commands continue to print a login hint when authentication is missing.

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

Piped stdin joins the prompt. `-p -` reads the whole prompt from stdin. On a
terminal a spinner shows the run status while you wait. Ctrl-C stops the wait
and prints the command that resumes it; the run continues on the server.

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
```

The command requires `cloudthinker login`. It starts the bundled local Pi agent in the checkout with read-only `read`, `grep`, `find`, and `ls` tools. CloudThinker supplies model inference, so model requests and responses pass through its gateway and normal observability traces. The local Pi transcript and findings are not mirrored as Agent CLI session entries or saved chat messages; the session metadata uses a generic title and cwd. Findings are validated against changed paths and lines and printed here. It does not create a source-control review, post comments, apply fixes, or change the Git index or worktree. `--timeout <secs>` limits the local agent run; rerun the command after a timeout.

Prepare Loupe on a shared Worker:

```sh
cloudthinker review setup --worker <WORKER_NAME_OR_ID>
cloudthinker review doctor --worker <WORKER_ID>
```

Setup starts or resumes a Worker-bound CloudThinker conversation. Follow its link to see the agent's work and answer questions. If the workspace has one shared Worker, you can omit `--worker`. Doctor checks Worker prerequisites; pass `--connection <ID> --provider <PROVIDER> --repository <PATH>` with `--worker` to also see the stored setup observations for that repository.

Inspect a CloudThinker review of a merge request or pull request by its URL:

```sh
cloudthinker review status <MR_URL>
cloudthinker review findings <MR_URL>          # worst severity first
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

## Environment

| Variable | Effect |
| --- | --- |
| `CLOUDTHINKER_URL` | API base URL. Defaults to the origin of your last `login --url`, else `https://app.cloudthinker.io`. Pass the bare origin. |
| `CLOUDTHINKER_LOGIN` | `browser` or `device` overrides the login flow that `login` picks for this machine. |
| `CLOUDTHINKER_WORKSPACE` | Same as `--workspace`. |
| `CLOUDTHINKER_TOKEN` | Uses this bearer instead of stored credentials. Cannot be combined with `--workspace`. |
| `CLOUDTHINKER_NO_UPDATE_CHECK` | Turns off the start-up update offer and the update notice. |

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
