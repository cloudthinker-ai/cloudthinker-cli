# Local Cyber execution adapter

The shared investigation contract above owns the lifecycle and proof standard.
This adapter maps it to the local tools. All target traffic runs on this machine.
Do not use cloud sandbox paths, cloud-only `cyber` helpers, `cyber_timeline` or
`ct_cyber` in local assignments.

## Assign roles through the workflow

Use `ct_workflow` with named-object inputs and actual JSON `args`. Start every
script with its literal `export const meta = { name, description, phases }`.
Use `agent(prompt, { agentType: "general-purpose", label, schema, receipt: true })`
for each bounded assignment. It returns `{ result, execution }`: `result` is the
schema-validated role output, while `execution` is the runtime-owned record with
`record_id`, optional linked `conversation_id` and `resumed`. Save both parts;
read role fields from `result`, not from the envelope. A role belongs in its explicit assignment and
label; do not invent an installed `cyber-hunter` local agent type.

Use a fresh `agent()` call for every producer, technical verifier, final-record
reviewer and Critic execution. `resume` continues the same context, so it cannot
provide an independent review of its own work. Read existing workflow journals
and use `resumeFromRunId` to retain unchanged completed work after interruption.
A null result is missing work, not a negative finding.

Build a JSON Schema for each call, with required fields and finite enums for
role, assignment and verdict. Save detailed evidence in the assigned files and
return compact structured references:

| Role | Required output | Coordinator action |
| --- | --- | --- |
| Recon | `role: "recon"`, `report`, `surface_files`, `journeys`, `identity_gaps`, `limitations` | Read mapping, upload normalized surface and refresh the plan |
| Hunter | `role: "hunter"`, `report`, `row_ids`, `candidates`, `limitations` | Read observations and assign each ready candidate for verification |
| Verifier | `assignment: "technical"`, `claims` with `candidate`, `decision` (`confirmed`, `refuted`, `inconclusive`) and `report` | Read proof and adjudicate each claim; never infer missing members |
| Record reviewer | `assignment: "final-record-review"`, `report`, `finding`, `revision`, `discrepancies` | Repair the exact adjudicated record; send material changes for fresh proof |
| Critic | `role: "critic"`, `report`, `gaps` with `subject`, `reason`, `next_role` (`recon`, `hunter`, `verifier`) | Accept, reject or defer each gap with its reason |

Worker output paths are references, not proof the files exist. Read each returned
report and its artifacts. Actual runtime execution receipts stay separate from
model-authored role results. Preserve producer `execution.conversation_id` values
and each verifier's distinct `execution.conversation_id`. Verification and final
record review require fresh executions, with `execution.resumed` false. Never
ask a worker to manufacture an execution ID. Missing conversation linkage is an
independence limitation; leave affected claims unresolved until it is repaired.
A model-written UUID or renamed role cannot repair that gap.

Recon and Hunters may read `cloudthinker --skill cyber-scan`; Verifiers read
`--skill cyber-verify`. Supply each assignment's target, frozen scope, identity
variable names and revisions, relevant source/context paths, row IDs, producer
references and distinct evidence/report paths. Keep credentials out of prompts.
Serialize shared login mutation; allow independent identities and read-only
mapping to run concurrently. Only the coordinator adjudicates and reports.

## Plan and adapt

Use `cloudthinker cyber probe plan <run-id>` after reconciling discovery and Recon
surface. Add newly observed surface evidence before refreshing it. The backend
plan bounds recorded inventory, not the hunt. Compare it with business journeys,
source and supplied API contracts; carry missing scope as an explicit limitation.
Use `probe partition` to assign focused rows and questions to Hunters.

Use `probe exec` for supported checks; use an authorized local method when it
cannot express the experiment. Preserve its evidence and record the outcome
through `probe ingest`. Record `covered` only with an assessed check and a
resolvable observation, `candidate` when an unsafe effect needs verification,
and `blocked` or `skipped_with_reason` with the actual reason. A failed probe
cannot establish a negative result. Keep evidence redacted.

Pipeline each ready candidate through a fresh Verifier. Join results before
adjudication; use a separate fresh final-record reviewer for high-severity or
contested findings. Run a Critic after the initial plan, joined evidence and,
for Ultra, before closure. Feed accepted gaps back into the appropriate stage.
Refresh `probe coverage` after new surface and outcomes. Preserve completed work
across passes and record useful next questions in the checkpoint handoff.

## Join and checkpoint

Upload completed surface, findings and evidence before checkpointing. Hash the
exact uploaded finding and verifier report bytes with SHA-256. Build each
verification receipt from actual workflow execution envelopes:

```json
{
  "finding_path": "findings/open/example.md",
  "finding_sha256": "<digest of uploaded finding bytes>",
  "producer_conversation_ids": ["<producer execution.conversation_id>"],
  "verifier_conversation_id": "<fresh verifier execution.conversation_id>",
  "report_path": "findings/evidence/example-verification.md",
  "report_sha256": "<digest of uploaded verifier report bytes>"
}
```

For high or critical severity, or a contested record, add the separate final
record review's `reviewer_conversation_id`, `review_report_path` and
`review_report_sha256`. Keep the final reviewed finding revision unchanged.
A verifier report must prove the exact claim; hashes bind bytes, not truth.
The backend checks linked child contexts, distinct executions, actual model
usage and uploaded artifact digests. Do not supply labels in place of IDs.

Read the current generation from saved Run state or the last checkpoint receipt.
Save the handoff with completed work, useful negative results, blocked identities
and the next valuable question. Use the current command's `--help` for flags:

```bash
cloudthinker cyber run checkpoint <run-id> --input <checkpoint.json> --json
```

The input JSON contains `generation`, optional `handoff` text, `verification`
receipts, `continue_requested` and optional `momentum`. Read the saved brief with
`cyber run bind <run-id> <conversation-id> --json` after an interruption.
Start at generation 0 when no checkpoint exists.

When another Ultra pass has useful authorized work, provide the Momentum
assessment and continuation request through the checkpoint options. The server
owns the decision and remaining allowance. Read `outcome`, `reason`,
`report_ready`, `gaps`, `next_generation` and `handoff` from its response.

- `continue`: retain the same Run, use `next_generation`, and start fresh worker
  context from saved records and the returned handoff. Reuse completed work.
- `close` with `report_ready=false`: repair each returned `code` using its
  `next_action`, upload changed records, then checkpoint the same generation.
- `close` with `report_ready=true`: load `--skill cyber-report`, build and inspect
  the final document, upload it and settle. Do not restart target testing merely
  because closure returned a repair gap.

Checkpoint retries preserve the saved decision. Never increment a generation,
assert readiness or invent a continuation allowance locally. A changed finding
needs current independent proof and digests before another ready checkpoint.
