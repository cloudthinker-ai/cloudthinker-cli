# Local Cyber pentests

Use this guide when the user asks to pentest a target reachable from their
machine. Handle setup and commands yourself, and explain progress and findings
in the current CloudThinker session. Use command `--help` for flags and syntax.
Call the assessment a run in prompts, progress, findings, and reports.

Commands run on this machine. Upload evidence with `cyber run evidence`;
CloudThinker stores the App, run, findings, and report. Session messages and
tool results also sync, so keep secrets out of their output.

## Prepare

- Read `.cloudthinker/config.toml`, the relevant source or API context, and any
  existing run. Keep the user's target and scope explicit.
- Run `cloudthinker cyber doctor`; use `--fix` when the probe toolpack needs
  repair. Save confirmed context or identity references with `cyber config set`.
  Preserve unrelated settings. Ask only for information you cannot establish.
- Check target reachability, expected authentication, the local runtime and
  required tools before discovery. Repair setup where possible and record limitations.
- `[cyber.context]` holds useful paths or URLs, such as `api_spec` or `notes`.
  `[cyber.auth]` maps roles to environment-variable names. Keep credentials out
  of config, prompts, reports, and logs.
- Use the configured identities that fit the check. For authorization tests,
  compare the same resource across identities against its expected access rules;
  matching HTTP status codes alone do not establish a vulnerability.
  Establish those rules from the supplied policy, API contract, or source,
  including global administrator privileges and supported parameters.

For a new run:

```bash
cloudthinker cyber run open --name <app-name> --target <url> --conversation-id <this-session-id>
```

This selects or creates the App, checks domain proof, saves its selection, and
binds a run to the session. An existing draft may need further setup. Mode and
scan focus are derived; choose intensity to match the authorized scope.
Use `--include` and `--exclude` when opening a run with narrower path scope;
these become discovery constraints. Context notes alone do not constrain probes.
For a continuation, inspect the existing run and reuse its completed work.
The brief's `local_workspace` gives stable paths: `evidence_root` for evidence
and report files, `workflow_root` for scripts and journals, and `workspace_root`
for working notes. Rebinding the same run preserves these files.
Load prior findings and surface with `cyber memory pull <app-id> --output <dir>`.
Add `--include-context` for attached files; metadata excludes credentials.
Treat them as context: prior coverage does not prove this run tested a row, and
a finding is resolved only after its original proof no longer reproduces.

## Discover

Start `cloudthinker cyber discover <run-id>` with `ct_background`. The command
runs the shared collectors locally and uploads surface and a health report.
Continue any useful work while it runs, including workflow stages whose inputs
are ready. Its completion event tells you when to read the output; only work
that needs those results should wait. Discovery needs no LLM worker.

Save URL-based API specs as local context files first. The context collector
parses specs; the source collector reads code. Reconciliation may drop candidates.
Use partial results and describe blind spots. Repair or rerun failed discovery
without discarding the rest of the work.

## Execute through a workflow

Use discovery results and the supplied context to plan the pentest, then run it
through `ct_workflow`. Read `--skill cyber-scan` for investigation,
`--skill cyber-verify` for candidates, and `--skill cyber-report` for reporting
as needed. The workflow owns worker limits,
progress, and completion events. Give it the bound Cyber run id, target, context paths,
evidence directory, and identity variable names.
Report its task and progress in plain language.

Choose stages that fit the work: scout, scan, verify candidates independently,
and assemble the result. The main agent chooses Light, Pro, or Ultra for each
worker's task. Use parallel workers where useful, with clear
assignments and bounded work. Give each worker its scope and expected evidence.
Retry, reassign, or record a limitation when a worker fails. Verification demonstrates
a violated security rule, beyond reproducing the candidate's response.

Request a plan from the available surface. Incorporate discovery results and
additional surface evidence before refreshing it and checking final coverage.
Plans record coverage without sending probes. Record untested scope with reasons.
Use the plan and coverage commands to keep the work recorded:

```bash
cloudthinker cyber probe plan <run-id>
cloudthinker cyber probe exec <run-id>
cloudthinker cyber probe coverage <run-id>
```

Use `probe partition`, `--rows`, `--identity`, and `probe ingest` as needed for
assigned work and observations. Prefer tracked probes; where another tool is
needed, retain its evidence and record the outcome. Check that planned coverage
reflects the supplied context. A small completed plan does not establish that
a large API was covered; describe omitted scope and blockers honestly.

## Keep evidence usable

Upload paths relative to the local evidence directory:

- `surface/<asset-type>.ndjson`: each entry has `asset_type` and `locator`.
- `findings/candidates/open/<slug>.md`: an unverified lead; not a published finding.
- `findings/open/<slug>.md`: independently verified findings. Other outcomes
  use `resolved/`, `needs-verification/`, or `dismissed/`.
- `findings/evidence/<path>`: supporting proof, observations, and attachments.
- `INDEX.md`: summary, tested scope, findings, and limitations.

`--skill cyber-verify` includes the published finding format. Upload completed
evidence incrementally; keep unfinished notes and report drafts in the workspace.

## Report

Use the run brief's frozen report preferences and reference, when present.
Generate a real PDF or DOCX locally after verification and coverage checks.
Include scope, confirmed findings and redacted proof, remediation, discovery
limits, tested versus skipped coverage, and prior findings not retested.
Validate the document before saving `output/report.pdf` or `output/report.docx`
in the evidence directory, then upload it with the other evidence.

## Finish or recover

Upload evidence with `cloudthinker cyber run evidence <run-id> <paths...>`.
Check coverage, unresolved candidates, and findings before `cyber run settle`.
Zero findings can still be a successful run. Confirm the returned run state
before reporting completion.

Keep drafts, evidence, and workflow files as work progresses. Esc interrupts
the current turn; it is not a request to end the Cyber run. On “Continue,” read
the run state and saved work, then resume what remains. Retry repairable errors
without discarding completed work. If the run has ended, reuse useful drafts
in a new run for the same App. Use `run cancel` when the user ends the run,
or `run settle --failed --message '<reason>'` when it cannot continue.
For authentication trouble, read `cloudthinker --skill auth` and repair access.
