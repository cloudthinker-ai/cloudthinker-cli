---
name: tour
description: 'Use on "show me around", "how does this CLI work", or "explain Cloud and Local" in the CloudThinker CLI.'
---

# CloudThinker CLI tour

Guide the user to a useful first task in their current session. Match their language and keep the first answer under 180 words. Use two short paragraphs or bullets plus the task choices.

## Start with the two places

Explain in two short bullets:
- **Local [L]** is the user's current directory on their machine. The agent can read and edit files and run local commands.
- **Cloud [C]** is the workspace's shared CloudThinker Sandbox. It reaches the workspace's connected services using credentials kept in the cloud. This machine does not receive those credentials.

The useful difference is one conversation that can connect local code with cloud evidence: for example, compare a repository's deployment configuration with the cluster it runs on, or investigate an error using both source code and connected logs.

Use the current session context to say whether Cloud is on and which Connections are available. If that information is unavailable, say so and point to `/cloud` for status. Explain that `/where` shows the two locations. Never infer a connection or successful access from a label.

## Offer one first task

Offer at most three concrete choices adapted to what is available:
- Local: explain this repository's entry points or inspect its deployment configuration.
- Cloud: inspect a named connected service with a bounded read.
- Both: compare local deployment configuration with a connected environment.

For `/skill:tour` without a requested task, explain and offer the choices, then wait. Do not run a fixed demo or inspect files just to fill the tour. If the user already names a task, explain the relevant place briefly and start that task using normal permission rules.

When Cloud is off, offer a local task and explain that `/cloud on` enables remote tools. Do not switch it on yourself. When no Connection is available, offer a local task and point the user to Connections in the workspace; never request a credential, kubeconfig, or token. When Cloud is unavailable or denied, explain the limitation and keep the local option usable.

## Explain the controls when relevant

- `/cloud` shows workspace and Connection status; `/cloud on|off` controls cloud tools for this session. Off does not cancel remote work already running.
- Cloud reads run in the sandbox; cloud changes follow the workspace's approval policy. Local writes follow local permissions.
- Successful sandbox output is collapsed by default. `Ctrl+O` expands tool details and output; failures keep a short preview. Collapsing affects the display, not what the agent receives.
- The CLI can delegate a cloud task to Anna with `ct_ask` and follow it with `ct_run_status`. Delegation may produce changes, so keep onboarding to explanations and requested bounded reads.
- Outside the session, `cloudthinker chat -p "<task>"` asks Anna from a script and prints her answer. It is a separate cloud run, not this local agent.
- This tour ships with the CLI's agent bundle. `cloudthinker update` followed by a new agent session loads the released version.

Mention only the controls needed for the user's next step. Do not list every slash command or describe internal architecture. Do not claim a command ran, a connection works, or a change is approved without evidence.

For the full command guide, link https://docs.cloudthinker.io/guide/cli/overview when the user wants more detail. The bundled instructions must work without fetching that page.
