## Additional changes for 0.7.6

- Replace the fixed tour command with a bundled /skill:tour that explains Local and Cloud and helps users choose a first task.
- Keep automatic skill metadata within an 8 KiB context budget while preserving search and explicit invocation of every discovered skill.

## [0.7.6]

- Adds session-owned local background shell commands with bounded output cursors, status, cancellation, timeout, recovery, and event-driven completion.
- Report execution errors as failed tool calls so agents can recover and session traces retain the outcome.
- An active workflow start with the same source and JSON arguments reuses its task ID; completed workflows remain rerunnable.
- Stabilize background recovery coverage by waiting for the running task's output to be persisted before restoring its snapshot.
- Remove completed commands from the background widget as they finish, clearing it after the final running command; retain task status, output, and completion events.
- Expose the shared workflow engine as `ct_workflow`, share a configurable limit across each workflow and its nested steps (eight by default), and recover interrupted work from its saved script and journal.
- Record failed headless workflows as tool errors so retries and session history reflect the actual outcome.
- Start requested workflows directly and choose advertised Light, Pro, or Ultra tiers to fit each worker's task.
- Require optional `ct_workflow` arguments to be objects and reject invalid values before any workflow agents start.
- Set the workflow concurrency default to 8, cap validated session overrides at 8, and share the configured limit with nested workflows.

## [0.7.3]

- Add a private local review launch mode that runs without cloud tools or subagents.
- Add `/exit`, `/clear`, and `/config` aliases for `/quit`, `/new`, and `/settings` in the interactive agent.

## [0.7.1]

- Render Markdown tables in the terminal with open columns and restrained separators.

## [0.5.9]

- A new session now starts with Cloud off when the agent's `settings.json` sets `cloudDefault` to false, and a delegated child inherits that state even without a recorded session choice.
- Startup now names the two machines beside the inventory line, and every local tool call carries an ASCII `[L]` tag.
- Local tool calls draw their `[L]` legend from the rendering session's own state, so a delegated child and its parent never claim or suppress each other's first legend.

## [0.5.8]

- The agent now starts in pi's regular inline TUI by default instead of fullscreen; pass `--tui-mode fullscreen` or set the pi `tuiMode` setting to keep the fullscreen layout.

## [0.5.5]

- Validated bundled assets before packaging and checked pi integration contracts.
- Embedded the source build identity and added opt-in startup phase timing.
- Reduced measured local editor startup time with bytecode compilation.
- Bundled pi-subagents with CloudThinker-only agent modes, inherited defaults, and independent child session tracking.
- Waited for delegated work in print and JSON mode so CLI exit cannot cancel unfinished background children.
- Kept Pi at 0.85.1, the latest published release at verification.
- Preserved Cloud On for mention-clone children and saved child modes on reopen; made upstream description changes fail explicitly.
