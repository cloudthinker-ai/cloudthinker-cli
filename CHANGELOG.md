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
