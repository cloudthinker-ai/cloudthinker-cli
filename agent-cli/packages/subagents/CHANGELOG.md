## [0.13.0]

- Expose linked worker execution receipts to pentest workflows and preserve them when completed work is replayed.
- Sub-agent spinners and the fleet list follow the host tick, so they slow to 400 ms over SSH and `cloudthinker remote`.
- Shorten the Agent and `ct_workflow` tool descriptions and drop four system-prompt rules that repeated them, cutting about 1.9k tokens from every request without changing which tools exist.
- Internal refactor in this package: shared helpers replace duplicated code, with no user-visible change.

## [0.8.0-dev.4]

- Sub-agent mode guidance names CloudThinker Agent modes the same way as the rest of the CLI.

## [0.8.0]

- Sub-agents and workflows now build from CloudThinker's own source package instead of a patched npm package; their behavior is unchanged.
- Agent mention rows say they are agents, so the `@` picker can filter them.
- ↓ opens the task panel with the first task selected, so `x` stops it at once.
- An `@agent` mention still briefs the agent from the whole conversation and the live system prompt on pi 1.0.0.
