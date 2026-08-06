# cloudthinker

The CloudThinker command-line interface: browser login plus headless `chat`, `review`, and job-runner commands over the CloudThinker backend.

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
cloudthinker chat -p "..."  # headless one-shot
cloudthinker review <MR_URL> status
```

When an account can access several workspaces, login asks which workspace to
authorize. Each workspace credential remains available for the same host:

```sh
cloudthinker --workspace Production whoami
cloudthinker --workspace 11111111-1111-4111-8111-111111111111 chat -p "..."
cloudthinker logout                 # selected or active workspace only
cloudthinker logout --all           # every workspace for this host
```

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

This workspace is the root of the standalone GitHub repo
`cloudthinker-ai/cloudthinker-cli`, a **publish mirror** of the `cli/` tree in the
GitLab monorepo (the source of truth). Never edit here directly — changes land in the
monorepo and are pushed with `make -C cli release-sync`. A release is a pushed semver
tag:

```sh
# bump `version` in crates/cloudthinker-cli/Cargo.toml, commit, then:
git tag v0.1.0 && git push origin v0.1.0
```

The tag triggers `release.yml`, which cross-builds every target and publishes a
GitHub Release carrying the platform archives plus `cloudthinker-cli-installer.sh`
and `cloudthinker-cli-installer.ps1`.

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
