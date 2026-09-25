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
