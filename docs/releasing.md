# Releasing

Distribution is [cargo-dist](https://opensource.axo.dev/cargo-dist/) driven; config
lives in `dist-workspace.toml`, the release pipeline in
`.github/workflows/release.yml`. The workflow is **hand-maintained**: it was
born from `dist generate` but carries deliberate hardening that regeneration
would revert — least-privilege permission scoping and inter-job artifact
transport through `actions/cache` under run-scoped keys instead of GitHub
Actions artifact storage (that storage hit a stale quota error in 2026-09,
APT-1002). `allow-dirty = ["ci"]` in `dist-workspace.toml` keeps dist's
consistency check from rejecting the divergence. Edit the
workflow by hand and mirror every deliberate change into it after a `dist`
upgrade.

This workspace is the root of the public GitHub repo
`cloudthinker-ai/cloudthinker-cli`, a **publish mirror** of the `cli/` tree in the
GitLab monorepo (the source of truth). Never edit here directly. Changes land in the
monorepo and are pushed with `make -C cli release-sync`. A release is a pushed semver
tag on this repo:

```sh
# bump `version` in crates/cloudthinker-cli/Cargo.toml, commit, then:
git tag v0.1.0 && git push origin v0.1.0
```

The tag triggers `release.yml`, which cross-builds every target and publishes a
GitHub Release on this repo with the runner `GITHUB_TOKEN`, carrying the platform
archives plus `cloudthinker-cli-installer.sh` and `cloudthinker-cli-installer.ps1`.
The sync leaves every `AGENTS.md` and `CLAUDE.md` out of this repo.

The release also carries `cloudthinker-cli-x86_64-pc-windows-msvc.zip` and the
PowerShell installer, and the README does not announce them. Windows has no
`cloudthinker-agent` bundle (`crates/cloudthinker-client/src/agent_release.rs`
maps macOS and Linux only), so `cloudthinker agent`, the command a bare
`cloudthinker` runs, fails there. Announce Windows once that bundle ships.

## Vanity install URL

`https://cloudthinker.io/install.{sh,ps1}` is a 307 to the latest release's
installer, so the published one-liner never pins a version:

```
cloudthinker.io/install.sh  -> github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.sh
cloudthinker.io/install.ps1 -> github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.ps1
```

The redirect is wired at the Vercel project serving `cloudthinker.io`; nothing in
this repo serves it. [cdn-install-redirect.md](cdn-install-redirect.md) holds that config.

Homebrew is deferred: adding a `"homebrew"` installer needs a separate
`cloudthinker-ai/homebrew-tap` repo and a tap entry in `dist-workspace.toml`.
