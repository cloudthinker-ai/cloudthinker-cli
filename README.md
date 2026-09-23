# cloudthinker

The CloudThinker command-line interface. Log in through your browser, run the
CloudThinker coding agent on your own machine, and drive Anna, code reviews, and
jobs headlessly from any shell or CI runner.

## Agent skill

Read the release-matched usage skill directly from the installed binary:

```sh
cloudthinker --skill
cloudthinker --skill auth
cloudthinker --skill chat
cloudthinker --skill review
```

The hub holds shared rules and routes tasks to focused modules. Each command prints
Markdown and exits without login, network access, or starting the local agent.
To make the skill discoverable by a coding agent, copy the bundled
`crates/cloudthinker-cli/skills/cloudthinker-cli/` directory into that agent's skill
search path. A hub-only installation can also load every module through the CLI.
For example, to install the hub in a project's shared agent skills directory:

```sh
mkdir -p .agents/skills/cloudthinker-cli
cloudthinker --skill > .agents/skills/cloudthinker-cli/SKILL.md
```

## Install

```sh
curl -fsSL https://cloudthinker.io/install.sh | sh
```

macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64). The installer drops
the `cloudthinker` binary in `~/.local/bin` and adds that directory to your
`PATH`, so open a new shell afterwards.

Keep it current with `cloudthinker update`. On an interactive terminal the CLI
also offers the update itself when a newer release exists; set
`CLOUDTHINKER_NO_UPDATE_CHECK=1` to silence that.

## First session

```sh
cloudthinker login     # browser PKCE login; pick a workspace if you have several
cloudthinker whoami    # the live host, account, and active workspace
cloudthinker           # the bare command runs the agent in this directory
```

On a machine with no browser, `login --no-browser` prints the consent URL and
`login --device-auth` switches to a short code instead of a loopback callback.

## The local coding agent

`cloudthinker agent` runs the CloudThinker coding agent where you are: it edits
files and runs shells on your machine, while the model and the workspace
Connections stay in the cloud. A bare `cloudthinker` is the same command.

```sh
cloudthinker agent
cloudthinker agent -p "Add a health check to main.tf" --model cloudthinker/pro
cloudthinker --workspace Production agent          # the CLI's own options come first
cloudthinker agent -- --url http://localhost:3000  # after `--`, everything is the agent's
```

Every argument after `agent` reaches the agent verbatim. The first run downloads
the agent build for your platform from the same GitHub release as this binary,
checks it against the release's SHA-256 sidecar, and installs it under
`~/.cloudthinker/agent/bin/<version>/`. Later runs reuse it, and a version bump
replaces it.

`agent` is the one command that starts a login on its own, because it is the
first thing a new user runs. Every other command prints the login command
instead.

## Headless chat

`chat -p` submits a prompt to Anna and waits for the answer. stdout carries only
Anna's final answer, so it stays pipeable; progress and continuation hints go to
stderr.

```sh
cloudthinker chat -p "Check production health"
cloudthinker chat -p "Draft the rollout plan" --json
```

Continue a thread with either a run UUID or a conversation UUID. The CLI prints
`continue_with=<conversation_id>` on stderr after each terminal run:

```sh
cloudthinker chat -p "Remove the risky step" --continue <run-or-conversation-uuid>
```

Submit without waiting, collect the run later, or recover an ID from recent runs:

```sh
cloudthinker chat -p "Audit production" --no-wait --json
cloudthinker chat status <run-uuid> --wait
cloudthinker chat ls --limit 10
cloudthinker chat ls --conversation <conversation-uuid> --json
```

`--no-wait` prints the submitted run identifiers because no answer exists yet.
`--timeout <secs>` bounds the client's wait only; the run continues server-side.

## Code review

Inspect a review CloudThinker tracks for a merge request or pull request, by its
URL:

```sh
cloudthinker review status <MR_URL>
cloudthinker review findings <MR_URL>          # worst severity first
cloudthinker review watch <MR_URL> --json      # poll to a terminal state
```

## Workspaces and credentials

When an account can reach several workspaces, login asks which one to authorize.
Each workspace credential stays available for the same host, and `--workspace`
selects between them by workspace ID or exact name:

```sh
cloudthinker --workspace Production whoami
cloudthinker --workspace 11111111-1111-4111-8111-111111111111 chat -p "..."
cloudthinker logout                 # the selected or active workspace only
cloudthinker logout --all           # every workspace for this host
```

A tool that needs its own bearer reads one from the CLI. `auth token` prints the
current access token on stdout and nothing else, refreshing it first when it is
close to expiry:

```sh
cloudthinker auth token
```

Treat that value as a secret: it authenticates as you until it expires.

Credentials live in `cloudthinker/credentials.json` under the operating system's
config directory, mode 0600.

## Environment

| Variable | Effect |
| --- | --- |
| `CLOUDTHINKER_URL` | API base URL. Defaults to `https://app.cloudthinker.io`. Pass the bare origin. |
| `CLOUDTHINKER_WORKSPACE` | Same as `--workspace`. |
| `CLOUDTHINKER_TOKEN` | Uses this bearer instead of stored credentials. Cannot be combined with `--workspace`. |
| `CLOUDTHINKER_NO_UPDATE_CHECK` | Turns off the start-up update offer. |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | The job failed: a FAILED run, an unknown run, or exhausted transport retries. |
| 2 | Bad usage, or a server-side validation or secret-gate rejection. |
| 3 | Not logged in, or the credential expired. |
| 4 | A client deadline elapsed. The run continues server-side. |
| 5 | The run paused for human approval in the browser. |

Documentation: <https://docs.cloudthinker.io>

## Releasing (maintainers)

Distribution is [cargo-dist](https://opensource.axo.dev/cargo-dist/) driven; config
lives in `dist-workspace.toml`, the release pipeline in
`.github/workflows/release.yml`. The workflow is **hand-maintained**: it was
born from `dist generate` but carries deliberate hardening that regeneration
would revert — least-privilege permission scoping, an external release repo,
and inter-job artifact transport through `actions/cache` under run-scoped keys
instead of GitHub Actions artifact storage (that storage hit a stale quota
error in 2026-09, APT-1002). `allow-dirty = ["ci"]` in `dist-workspace.toml`
keeps dist's consistency check from rejecting the divergence. Edit the
workflow by hand and mirror every deliberate change into it after a `dist`
upgrade.

This workspace is the root of the public GitHub repo
`cloudthinker-ai/cloudthinker-cli-src`, a **publish mirror** of the `cli/` tree in the
GitLab monorepo (the source of truth). Never edit here directly. Changes land in the
monorepo and are pushed with `make -C cli release-sync`. A release is a pushed semver
tag on the source repo:

```sh
# bump `version` in crates/cloudthinker-cli/Cargo.toml, commit, then:
git tag v0.1.0 && git push origin v0.1.0
```

The tag triggers `release.yml`, which cross-builds every target and publishes a
GitHub Release on the public repo `cloudthinker-ai/cloudthinker-cli` carrying the
platform archives plus `cloudthinker-cli-installer.sh` and
`cloudthinker-cli-installer.ps1` (`github-releases-repo` in `dist-workspace.toml`;
the source repo holds the `GH_RELEASES_TOKEN` secret that writes there). The releases
repo carries releases only, and the source repo carries the code; both are public.
The sync leaves every `AGENTS.md` and `CLAUDE.md` out of the source repo. The releases repo's README is not mirrored; the copy to publish by hand lives in
`docs/release-repo-README.md`.

The release also carries `cloudthinker-cli-x86_64-pc-windows-msvc.zip` and the
PowerShell installer, and neither README announces them. Windows has no
`cloudthinker-agent` bundle (`crates/cloudthinker-client/src/agent_release.rs`
maps macOS and Linux only), so `cloudthinker agent`, the command a bare
`cloudthinker` runs, fails there. Announce Windows once that bundle ships.

### Vanity install URL

`https://cloudthinker.io/install.{sh,ps1}` is a 307 to the latest release's
installer, so the published one-liner never pins a version:

```
cloudthinker.io/install.sh  -> github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.sh
cloudthinker.io/install.ps1 -> github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.ps1
```

The redirect is wired at the Vercel project serving `cloudthinker.io`; nothing in
this repo serves it. `docs/cdn-install-redirect.md` holds that config.

Homebrew is deferred: adding a `"homebrew"` installer needs a separate
`cloudthinker-ai/homebrew-tap` repo and a tap entry in `dist-workspace.toml`.

## License

The CloudThinker CLI is licensed under the [Apache License, Version 2.0](LICENSE).

The CloudThinker coding agent (`cloudthinker agent`) is built on the
[pi](https://github.com/earendil-works/pi) agent harness by Mario Zechner, which
is licensed under the MIT License. [NOTICE](NOTICE) lists pi and the other
third-party components with their license texts.
