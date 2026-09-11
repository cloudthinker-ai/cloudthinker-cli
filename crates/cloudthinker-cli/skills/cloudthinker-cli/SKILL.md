---
name: cloudthinker-cli
description: 'Use CloudThinker through its CLI to delegate to Anna, follow conversations, or inspect code reviews.'
---

# CloudThinker CLI

One entry point for using the customer-facing `cloudthinker` CLI. This hub holds shared rules; modules hold workflows for each surface. Read only the modules matching the task, and combine them when needed.

## Learn the installed CLI

Run `cloudthinker --skill` once per session to load the release-matched hub. If this text already came from that command, continue directly to the matching module. Read modules through the commands below; bundled Markdown links are also available when the full skill directory is installed. Prefer the installed binary when copies differ.

Use `cloudthinker --help` and `cloudthinker <command> --help` for exact syntax. Never run bare `cloudthinker` for discovery: it starts the local coding agent. The internal developer command `ct` is a different tool.

## Core invariants

- Establish the intended host and workspace with `cloudthinker whoami` before authenticated work. If the task selects a workspace, pass `--workspace <id-or-name>` consistently; never guess among duplicate names.
- Operate within the user's request. A prompt sent to Anna can execute cloud operations; describe the intended scope and require read-only investigation when that is the task. A loaded skill does not authorize additional changes.
- Use `--json` where the command supports it. Read identifiers and statuses from actual output; never invent an ID or select the latest conversation implicitly.
- A conversation owns the thread; a run owns one turn. Retain both IDs. A waiting timeout leaves the server run alive; follow it instead of resubmitting.
- Keep credentials private. Routine tasks use the CLI's stored login; do not print `auth token`, read credential files, or paste secrets into prompts.
- Report the observed result and its limits. Submission, successful execution, and proof of the requested outcome are separate facts.

## Modules

Read every module whose signal matches the task:

- `cloudthinker --skill auth` ([auth.md](auth.md)): login, identity, host selection, workspace selection, authentication errors.
- `cloudthinker --skill chat` ([chat.md](chat.md)): delegate to Anna, recover a run, continue a conversation, timeout or approval handling.
- `cloudthinker --skill review` ([review.md](review.md)): inspect the status or findings of a tracked merge request or pull request.

Typical combinations: first delegation = auth + chat. Review lookup in a named workspace = auth + review. Continuing an identified conversation = chat.

## Verify

Inspect the command's exit status and structured result. For delegated work, inspect Anna's answer and evidence against the original request. For Review, inspect the returned review status and findings; a successful lookup alone does not mean the review passed.

## Boundaries

This skill uses released CLI capabilities. Building the CLI, operating the local development stack, and running an interactive local coding agent belong to their own workflows. Discover additional supported modules with `cloudthinker --skill`; do not invent commands for product features missing from the CLI.
