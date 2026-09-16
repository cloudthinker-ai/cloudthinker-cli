# CloudThinker CLI

Release assets for the CloudThinker CLI and the CloudThinker Agent. The source
lives elsewhere; this repository carries downloads only.

## Install

```sh
curl -fsSL https://cloudthinker.io/install.sh | sh
```

macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64). The installer puts
`cloudthinker` in `~/.local/bin` and adds it to your `PATH`, so open a new shell
afterwards.

## Use it

```sh
cloudthinker login                  # browser login
cloudthinker                        # run the coding agent in this directory
cloudthinker chat -p "Check production health"
cloudthinker review status <MR_URL>
```

`cloudthinker` runs the CloudThinker coding agent on your own machine: it edits
files and runs shells locally, while the model and your workspace Connections
stay in the cloud. `chat -p` is the headless path for scripts and CI; stdout
carries only Anna's answer.

Update with `cloudthinker update`. Full documentation:
<https://docs.cloudthinker.io>
