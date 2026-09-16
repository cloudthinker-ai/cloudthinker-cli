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

