## [0.8.0]

- Open the terminal in fullscreen by default; `--tui-mode regular` or a saved TUI mode in `/settings` keeps the classic view.
- Ctrl+O opens a full-height transcript you can search with `/`, step through with `n`/`N`, and open in your editor with `e`, instead of expanding tool output in place.
- Scrolling up shows a `↓ N new` pill for the output that arrived below.
- Queued messages wait above the editor in gray, each marked to send after this step or after this turn.
- Ctrl+L clears the screen instead of opening the model picker, which stays on `/model`; a running shell command shows one `⋯ running` line in compact output; `/exit`, `/clear`, and `/config` describe themselves with their target command's words.
- Thinking no longer takes chat rows: the working line reads `Thinking 4s` while the model thinks, Ctrl+T or `/settings` shows every thought, and the Ctrl+O transcript includes it.
- Adjacent `read`/`grep`/`find`/`ls` calls fold into one clickable row; `edit` and `write` show `+added −removed` until clicked; answer code blocks are numbered so `/copy N` copies one; the transcript copies a block with `y` or everything with `Y`; Ctrl+R searches past prompts; `@` ranks git files fuzzily with changed and recent files first.
- Ctrl+L refuses while a task runs; a shell command fits on one row and a stopped one reads `stopped`; `/copy` rejects a bad argument instead of sending it to the model; Ctrl+R starts from your draft; `@` skips deleted files.
- When the agent modes cannot load, the agent starts with no model instead of a vendor model, and a message sent before the cloud returns says to run `/cloud retry` instead of pointing at vendor login.
- A workflow under heavy CPU load no longer fails with a false "`meta` must be a literal" error: the `meta` evaluation bound is 1s instead of 100ms, and a runaway `meta` is still rejected.
- Tool and task rows say what happened in words instead of tick and cross marks: a success shows only its line count, a stop reads `stopped`, a failure reads `failed` or `error:`; a long command row keeps the tool box background behind its `...` and `+N lines`.
- The `[L] your machine - [C] CloudThinker Sandbox` legend line no longer repeats above the first tool call; the startup block explains both tags.
- Running agents and commands take one row above the editor; ↓ at an empty prompt opens the task panel, where Enter views a task and `x` stops it.
- Agents always run in the background in the terminal, and their launch and completion each take one row.
- A shell command still running after 10 seconds moves to the background without restarting; Ctrl+B moves one at once, and `CLOUDTHINKER_AUTO_BACKGROUND_SECONDS` sets the wait (`0` turns it off).
- Sub-agents and workflows now build from CloudThinker's own source package instead of a patched npm package; their behavior is unchanged.
- `@` lists your workspace Connections beside files and agents; ←/→ switches between All, Files, Connections, and Agents, and `#` opens Connections alone.
- A Connection inserts the web app's `#connection/…` token and a file always inserts as `@./path`, so a bare `@name` is always an agent.
- Mentions are drawn in the primary color in the editor and in sent messages, and a Connection the workspace lacks shows red.
- Ctrl+U and other kill edits close an `@` list they made stale.
- Imports the CloudThinker extension from its new name, `@cloudthinker/cloud`; behavior is unchanged.
- A finished background command shows as one row in chat (`pnpm test · finished in 16s`), and so does its queued preview.
- Background status, output, and cancel calls each take one row in `compact` output; click a row for the full view.
- Stopping a command with `x` tells the agent it was stopped by you, so it no longer waits for it or runs it again.
- Built on pi 1.0.0 (from 0.85.1), with pi's fixes from 15 releases.
- Startup still counts errors and warnings in its one-line summary, and large skill sets still list the full catalog for the agent.
- `cloudthinker agent --help` shows CloudThinker's own short help: the CloudThinker agent in your terminal, its modes, and real examples, with no third-party providers or API keys.
- Flags and commands a CloudThinker user does not need now stop with a clear next step (exit 2) instead of running or being sent to the model as a prompt: `--provider`, `--api-key`, a vendor `--model`, `--thinking`, `--system-prompt`, `--no-session`, `install`, `list`, `config`, `auth`, `mcp`, and `update` (use `cloudthinker update`).
- `-p` with no prompt in a terminal says how to pass one instead of exiting silently.
- The agent no longer sends pi's install ping to pi.dev.
- `/thinking` and `/scoped-models` are gone from the command list; `/model` picks light, pro, or ultra.
- The release bundle ships this agent's CHANGELOG.md.
- A test keeps the agent's `--help` text equal to the copy that `cloudthinker agent --help` prints without starting the agent.

## [0.7.7]

- Render completed Mermaid flowcharts as themed terminal diagrams with authored node border colors; keep interiors and text in terminal theme colors. Preserve source while streaming and when a preview cannot fit or uses unsupported syntax.
- Make authored Mermaid border colors configurable through `markdown.mermaidBorderColors` in settings.json, defaulting to false. Suppress colored fallback notices when color is disabled.
- Resolve the workflow source from the installed patched package so the parity test works across pnpm layouts.
- Show running agents and running background commands together in one Tasks pane, each with a spinner, elapsed time, and its latest activity.
- Render a background start as one line, show command output as a 5-line preview, and list completions with a status mark and duration.
- Fold successful local tool output to a one-line summary by default; `/verbosity` or the `toolOutput` setting switches to the 5-line preview.
- Draw Markdown code blocks with a language label and a bar instead of fence lines.
- Ask a side question with `/btw` while the agent works. The side thread answers from the conversation so far, including finished tool output, and the main turn keeps running.
- `/btw` now opens in the right half of the terminal beside the running agent, and Esc or Ctrl+C closes it at any time, with the hint shown in its footer.

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
