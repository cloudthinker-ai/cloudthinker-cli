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
| Give an AI agent the CLI's internal operating guide | `cloudthinker --skill` |

## Install

```sh
curl -fsSL https://cloudthinker.io/install.sh | sh
```

The installer supports macOS (Apple Silicon and Intel) and Linux (x86_64 and
arm64). It puts the `cloudthinker` binary in `~/.local/bin` and adds that
directory to your `PATH`. Open a new shell after the install.

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
```

`--no-wait` prints the IDs of the submitted run, because no answer exists yet.
`--timeout <secs>` limits only the client's wait. The run continues on the
server.

## Code review

Review local changes through CloudThinker and print the findings in your terminal:

```sh
cloudthinker review                         # staged, unstaged, and untracked changes vs HEAD
cloudthinker review --base origin/develop   # branch diff from the merge base, including dirty edits
cloudthinker review --json                  # one machine-readable result on stdout
cloudthinker review --fail-on high          # exit 6 when a high or critical finding exists
```

The command requires `cloudthinker login`. It starts the bundled local Pi agent in the checkout with read-only `read`, `grep`, `find`, and `ls` tools. CloudThinker supplies model inference, so model requests and responses pass through its gateway and normal observability traces. The local Pi transcript and findings are not mirrored as Agent CLI session entries or saved chat messages; the session metadata uses a generic title and cwd. Findings are validated against changed paths and lines and printed here. It does not create a source-control review, post comments, apply fixes, or change the Git index or worktree. `--timeout <secs>` limits the local agent run; rerun the command after a timeout.

Inspect a CloudThinker review of a merge request or pull request by its URL:

```sh
cloudthinker review status <MR_URL>
cloudthinker review findings <MR_URL>          # worst severity first
cloudthinker review watch <MR_URL> --json      # poll until the review finishes
cloudthinker review watch <MR_URL> --fail-on high
```

## Outposts

An outpost lets a CloudThinker conversation work in a directory on your
machine. The worker connects outward over HTTPS and opens no inbound port.

```sh
cloudthinker worker outpost create my-project
cloudthinker worker start --outpost my-project --workdir "$PWD" --concurrency 4
```

File tools stay inside the selected directory. Shell commands run under your OS
account and are not sandboxed. To keep an outpost available after the terminal
closes, run `cloudthinker worker service install`. It writes a systemd user unit
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
