---
name: cloudthinker-cli
description: 'Use CloudThinker through its CLI to delegate cloud work, follow conversations, inspect code reviews, or run a local Cyber pentest.'
---

# CloudThinker CLI

One entry point for using the customer-facing `cloudthinker` CLI. This hub holds shared rules; modules hold workflows for each surface. Read only the modules matching the task, and combine them when needed.

## Learn the installed CLI

This is an agent-facing operating guide. `cloudthinker --skill` and its modules are internal reference material; never tell the user to run them. The bare `cloudthinker` command opens the interactive agent where the user states a task in plain language. Do not call it recursively from an active session. Read only the matching module, and prefer the installed binary when copies differ.

Use `cloudthinker --help` and `cloudthinker <command> --help` for exact syntax when needed. The internal developer command `ct` is a different tool.

## Core invariants

- Establish the intended host and workspace with `cloudthinker whoami` before authenticated work. If the task selects a workspace, pass `--workspace <id-or-name>` consistently; never guess among duplicate names.
- Operate within the user's request. A delegated prompt can execute cloud operations; describe the intended scope and require read-only investigation when that is the task. A loaded skill does not authorize additional changes.
- In an interactive session, use human-readable output and keep command details out of the user's instructions. Use `--json` only when another program must consume the result. Read identifiers and statuses from actual output; never invent an ID or select the latest conversation implicitly.
- A conversation owns the thread; a run owns one turn. Retain both IDs. A waiting timeout leaves the server run alive; follow it instead of resubmitting.
- Keep credentials private. Routine tasks use the CLI's stored login; do not print `auth token`, read credential files, or paste secrets into prompts.
- Report the observed result and its limits. Submission, successful execution, and proof of the requested outcome are separate facts.

## Modules

Read every module whose signal matches the task:

- `cloudthinker --skill auth` ([auth.md](auth.md)): login, identity, host selection, workspace selection, authentication errors.
- `cloudthinker --skill cloud` ([cloud.md](cloud.md)): use Connections and MCP directly from your own agent, load guides and schemas, execute in cloud and follow approval.
- `cloudthinker --skill chat` ([chat.md](chat.md)): delegate cloud work, recover a run, continue a conversation, timeout or approval handling.
- `cloudthinker --skill review` ([review.md](review.md)): inspect the status or findings of a tracked merge request or pull request.
- `cloudthinker --skill cyber` ([cyber.md](cyber.md)): the agent's internal procedure for running a Cyber pentest on this machine.
- `cloudthinker --skill cyber-scan` ([cyber-scan.md](cyber-scan.md)): plan and record coverage for a local Cyber run.
- `cloudthinker --skill cyber-verify` ([cyber-verify.md](cyber-verify.md)): independently test a candidate before treating it as a finding.
- `cloudthinker --skill cyber-report` ([cyber-report.md](cyber-report.md)): validate evidence and report the local Cyber run.
- `cloudthinker --skill worker` ([worker.md](worker.md)): create an outpost, serve a local project, check availability, and drain before an update.

Typical combinations: first delegation = auth + chat. Review lookup in a named workspace = auth + review. Continuing an identified conversation = chat. Serving a local folder for workspace access = auth + worker + chat. For a local pentest, read cyber plus only the scan, verification, or report module needed for the current stage.

## Verify

Inspect the command's exit status and structured result. For delegated work, inspect the answer and evidence against the original request. For Review, inspect the returned review status and findings; a successful lookup alone does not mean the review passed.

## Boundaries

This skill uses released CLI capabilities. The user's workflow is the interactive `cloudthinker` session; build and local-stack operations are developer workflows. Read modules with `cloudthinker --skill <module>` when needed; do not make the user discover internal commands or invent commands for missing product features.
