# cloudthinker

The CloudThinker command-line interface: browser login, the local coding agent, plus headless `chat`, `review`, and job-runner commands over the CloudThinker backend.

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

macOS / Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://cloudthinker.ai/install.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://cloudthinker.ai/install.ps1 | iex"
```

The installer drops the `cloudthinker` binary in `~/.local/bin` and adds it to your
`PATH`. Then:

```sh
cloudthinker login          # browser PKCE login
cloudthinker whoami         # live host, account, and active workspace
cloudthinker chat -p "..."  # start a headless conversation
cloudthinker review status <MR_URL>
cloudthinker agent          # run the local coding agent in this directory
```

`agent` runs the CloudThinker coding agent on your machine: it edits files and
runs shells locally, while the model and the workspace Connections stay in the
cloud. Every argument after `agent` reaches the agent verbatim:

```sh
cloudthinker agent -p "Add a health check to main.tf" --model cloudthinker/pro
cloudthinker --workspace Production agent          # the CLI's own options come first
cloudthinker agent -- --url http://localhost:3000  # after `--`, everything is the agent's
```

The first run downloads the agent build for your platform from the same GitHub
release as this binary, checks it against the release's SHA-256 sidecar, and
installs it under `~/.cloudthinker/agent/bin/<version>/`. Later runs reuse it,
and a version bump replaces it. macOS and Linux only for now.

Continue a thread with either a run UUID or conversation UUID. The CLI prints
`continue_with=<conversation_id>` on stderr after each terminal run:

```sh
cloudthinker chat -p "Draft the rollout plan"
cloudthinker chat -p "Remove the risky step" --continue <run-or-conversation-uuid>
```

Submit without waiting, collect a run later, or recover an ID from recent runs:

```sh
cloudthinker chat -p "Audit production" --no-wait --json
cloudthinker chat status <run-uuid> --wait
cloudthinker chat ls --limit 10
cloudthinker chat ls --conversation <conversation-uuid> --json
```

Human `chat -p` output remains pipeable: stdout contains only Anna's final
answer. Progress and continuation hints use stderr. `--no-wait` prints the
submitted run identifiers because no answer exists yet.

When an account can access several workspaces, login asks which workspace to
authorize. Each workspace credential remains available for the same host:

```sh
cloudthinker --workspace Production whoami
cloudthinker --workspace 11111111-1111-4111-8111-111111111111 chat -p "..."
cloudthinker logout                 # selected or active workspace only
cloudthinker logout --all           # every workspace for this host
```

A tool that needs its own bearer reads one from the CLI. `auth token` prints the
current access token on stdout and nothing else, refreshing it first when it is
close to expiry:

```sh
cloudthinker auth token
```

Treat that value as a secret: it authenticates as you until it expires.

`CLOUDTHINKER_TOKEN` overrides stored credentials. Do not combine it with
`--workspace`. The file fallback lives in the operating system's config
directory under `cloudthinker/credentials.json`; the CLI uses the OS keyring
when available.

Supported targets: macOS (Apple Silicon + Intel), Linux (x86_64 + arm64), Windows
(x86_64).

## Releasing (maintainers)

Distribution is [cargo-dist](https://opensource.axo.dev/cargo-dist/) driven; config
lives in `dist-workspace.toml`, the release pipeline in
`.github/workflows/release.yml`. Both are generated — edit the config and rerun
`dist generate`, never hand-edit the workflow.

This workspace is the root of the private GitHub repo
`cloudthinker-ai/cloudthinker-cli-src`, a **publish mirror** of the `cli/` tree in the
GitLab monorepo (the source of truth). Never edit here directly — changes land in the
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
the source repo holds the `GH_RELEASES_TOKEN` secret that writes there). The public
repo carries releases only, so the source stays private while every download URL
stays public.

### Vanity install URL

`https://cloudthinker.ai/install.{sh,ps1}` is a redirect (301/proxy) to the latest
release's installer, so the published one-liner never pins a version:

```
cloudthinker.ai/install.sh  -> github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.sh
cloudthinker.ai/install.ps1 -> github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.ps1
```

Wire the redirect at the CDN/edge hosting `cloudthinker.ai`; nothing in this repo
serves it.

Homebrew is deferred: adding a `"homebrew"` installer needs a separate
`cloudthinker-ai/homebrew-tap` repo and a tap entry in `dist-workspace.toml`.
