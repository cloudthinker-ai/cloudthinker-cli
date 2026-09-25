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
