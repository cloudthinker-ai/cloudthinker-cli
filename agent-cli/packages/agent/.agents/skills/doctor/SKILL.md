---
name: doctor
description: 'Use after /doctor finds a problem, or on "check my setup".'
---

# CloudThinker CLI doctor

/doctor ran `cloudthinker doctor` and showed its output. Explain the problems and offer the fix. Answer in under 80 words: one line per problem, worst first, then at most one question.

## Read the output

The output follows this skill. The first line names the version and the server. A `✓` line lists the healthy areas. Each other row is one problem: a mark (`✗` error, `?` unknown, `!` warning, `-` skipped), a name, the summary, and after ` · ` the fix command and what was not checked. Run `cloudthinker doctor --json` yourself only when you need detail the rows do not give, such as an HTTP status or a Connection's test phase. Never guess a cause the output does not give.

## Fixes

- Login, server address and account: give the command for the user to run in another terminal, such as `cloudthinker login` or `cloudthinker login --url <address>`. Never run login yourself, never ask for a token, key or password, and never put one in a command.
- Connection: send the user to Connections in CloudThinker to edit or reconnect it. Do not test or change it from here.
- Release: `cloudthinker update`, then start the agent again.
- Agent settings: when a setting explains what the user sees, offer the change and call `ct_config set` only after the user says yes; they approve it again in the modal. `ct_config` changes only the settings it lists.
- After a fix, suggest /doctor again. Do not run it in a loop yourself.
