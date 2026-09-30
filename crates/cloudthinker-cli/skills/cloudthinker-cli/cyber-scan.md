# Local Cyber scan

Use the run's frozen target and scope. Read the business workflows, data and
authorization rules in the supplied source, API contract, and discovery results.
Model likely abuse paths as actor, capability, asset, trust boundary, state
change, proof, and impact; prioritize reachable paths with meaningful impact.

## Plan and adapt

Use `cloudthinker cyber probe plan <run-id>` to see backend-issued checks. Add
relevant surface evidence before refreshing the plan. Compare its rows with the
application context: a plan is a coverage ledger, not proof of whole-app coverage.
Use `probe partition` to group checks when parallel work helps; choose boundaries
that fit the application's workflows and available evidence.

Test expected rules, not response codes alone. For access boundaries, compare
the same resource or operation across relevant identities and an allowed control.
For state changes, inspect the resulting state and use a safe control. Adapt
follow-up checks to observed behavior, newly discovered endpoints, and source
evidence. Record useful evidence for both confirmed behavior and negative results.

Use `probe exec` for supported checks; use an in-scope local method where it
cannot express the test. Record `covered` only with evidence that the check was
assessed, `candidate` when an unsafe effect needs verification, and
`blocked`/`skipped_with_reason` with the reason. Keep evidence redacted. Refresh
`probe coverage` after updates and state remaining gaps or unresolved candidates.
