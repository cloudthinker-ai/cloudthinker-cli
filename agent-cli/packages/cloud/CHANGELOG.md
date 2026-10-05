## [0.8.0-dev.4]

- Messages and tool descriptions name CloudThinker Agent the same way everywhere; the cloud agent behind `ct_ask` is CloudThinker Agent in the cloud.
- Keep temporary subagents and ct_ask independent of specialist visibility; remove unused mode capability polling and session state while preserving selected custom-agent UUID inheritance.

## [0.8.0]

- Each `#connection/…` in a message resolves against the workspace's Connections, and the agent gets a short note saying which are connected, ambiguous, or missing.
- The prompt explains the three mention forms: `#connection/…`, `@` with a relative path, and a bare `@name` for an agent.
- `/new` and `/clear` show the new session at once instead of after about a second: the token and agent modes carry over, the cloud link finishes in the background under `linking cloud…`, and a prompt sent before then waits for the link.
- The CloudThinker extension package is now `@cloudthinker/cloud` in `packages/cloud`; behavior is unchanged.
- The footer is now a status line: workspace, Auto or Manual, credits, and Connections, with the folder and branch on the right and the agent mode and context use below.
- A cloud write waiting for approval opens one modal with the reasoning, Connections, full command, and browser link; `y` approves, `a` approves and trusts, `n` declines, `b` hands off to the browser, and Esc declines.
- The expanded startup help lists Ctrl+L to clear the screen and `/model` to select a model.
- The header shows `ctrl+o transcript` once, without a second startup-details line.
- The expanded startup help lists Ctrl+T to show or hide thinking.
- A failed cloud link says why (signed out, access denied, can't reach a host, server error) and what to do; offline and server failures retry automatically, and `/cloud retry` retries now.
- The status line, approvals, and mirror status say what happened in words instead of cross and pause marks: `offline: can't reach <host>`, `approval needed:`, `mirror offline (n pending)`.
- Cloud tool calls no longer draw the `[L]`/`[C]` legend line above the first call; the startup block explains both tags.
- `/cloudthinker workspace` lists every workspace you belong to, grouped by organization, with the current one and the ones you have no login for marked; picking a logged-in workspace switches the models and token, saves it as your default, and starts a new session there. An older server lists only your logged-in workspaces.
- Runs on pi 1.0.0; Cloud tools behave as before.
- `/share` copies this conversation's CloudThinker link, which only members of your workspace can open.
- `/login` and `/logout` sign in and out of CloudThinker; `/login` shows the sign-in link and code, relinks the session when it finishes, and Esc cancels it.
- `/changelog` shows the latest releases of this agent and links to docs.cloudthinker.io/changelog.
- `/bug [title]` opens a new issue on the cloudthinker-cli GitHub repository with your versions filled in and no conversation attached.
- The agent reads CloudThinker incidents with `list_incidents` and `get_incident`, the same tools the Claude connector uses. They load from the workspace after the session links; an older CloudThinker server without them keeps the current cloud tools.

## Additional changes for 0.7.6

- Replace the fixed tour command with a bundled /skill:tour that explains Local and Cloud and helps users choose a first task.

## [0.7.6]

- The agent asks the `cloudthinker` binary that started it for a token, instead of the first `cloudthinker` on `PATH`, and waits up to 2 minutes for a token refresh to finish.
- Route local pentest requests through the Cyber skill and the shared workflow runner, with session-bound runs and progress in the terminal.
- Resolve API credentials without blocking the terminal client, sharing concurrent refreshes and preserving safe failure messages.
- Distinguish local files from cloud workspace files and remote paths returned by APIs.
- Direct local shell searches to `rg` for contents and `fd` for file and directory names in the internal system prompt, with Cloud on or off; use scoped searches with ignore rules and fall back only when the preferred command is unavailable.

## [0.7.4]

- Upgrade fflate to 0.8.3 so a malformed ZIP64 skill archive cannot hang skill sync (GHSA-px8p-9vwx-vf98).

## [0.7.3]

- Add a local review session mode that uses CloudThinker inference without mirroring local review content, and confine its file tools and results to existing, non-ignored paths inside the checkout.

## [0.7.2]

- A collapsed `ct_sandbox_read` or `ct_sandbox_write` result now shows only the reasoning and a line count; press ctrl+o to see the output. An error result still previews its last lines.

## [0.5.9]

- New sessions now start with Cloud off when the agent's `settings.json` sets `cloudDefault` to false; a `/cloud on|off` in a session still overrides it, and delegated children inherit the parent's effective state.
- Cloud tools render under an ASCII `[C]` tag beside the `[L]` local tag, with one legend line per session and a `/tour` that runs one local read and one read-only sandbox read.
- A brand-new session with Cloud off performs no remote startup work: the conversation link the gateway needs is established lazily on the first model turn, once, while identity, Connections, mirror, memory and skills initialize in the background, so chat still works while Cloud stays off, and `/cloud on` initializes them on demand. Terminal control sequences and zero-width or bidi format characters are stripped from directory names, workspace names, identity fields, session URLs, and `/tour` output before they are drawn, every session including a delegated child draws its own one-time `[L]`/`[C]` legend, and `/tour` runs exactly one local read.

## [0.5.7]

- Reported an unconfirmed Sandbox write as `outcome_unknown` and told the agent not to replay it automatically.
- Every cloud tool call now names its side: the ct_ call lines lead with a `cloud` word instead of the cloud glyph, `/where` prints what the agent can see on your machine and in the cloud workspace, the session title and header mark the local and cloud sides, and the copy says the workspace machine instead of the Sandbox.

## [0.5.6]

- The startup banner renders the CLOUD THINKER wordmark in the brand gradient, stacked above the session identity lines, replacing the stretched quadrant cloud.
- On light themes the wordmark gradient darkens so every row stays readable against the light background.

## [0.5.5]

- Displayed the source build identity in the about panel.
- Added opt-in timing for model registration and session startup.
- Use the active header theme for the startup cloud logo while preserving monochrome output.
- Linked child sessions to their parent conversation and inherited Cloud Off while keeping independent transcripts and credit records.
