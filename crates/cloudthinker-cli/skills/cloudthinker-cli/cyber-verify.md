# Local Cyber verification

Apply the shared investigation proof standard above. This local adapter owns
artifact layout and the workflow result shape. Read the candidate, expected rule,
producer references, exact identity revision, target scope and assigned evidence
paths. Replay the claim and controls in this fresh execution.

Each task has one assignment, technical verification or final-record review.
Return the structured shape requested by `agent(prompt, { schema, receipt: true })`; missing proof
returns `inconclusive`, never a fabricated confirmation. Save evidence-bearing
reports before returning. For technical verification, return:

```json
{"assignment":"technical","claims":[{"candidate":"<candidate path>","decision":"confirmed","report":"<assigned report path>"}]}
```

Use `confirmed`, `refuted` or `inconclusive` per claim and include every assigned
member. The report records the expected rule, prerequisites, identity/revision,
redacted request, captured response or resulting state, allowed control, impact
and limitations. Technical verification does not adjudicate its own result.

A separate final-record reviewer reads the exact adjudicated revision without
editing it. Return `assignment: "final-record-review"`, `report`, `finding`,
`revision` and `discrepancies`. Return discrepancies as an array; an empty array
means no discrepancy was found within the reviewed evidence. It is not a new
runtime proof. Material claim changes require fresh technical verification.

Keep actual runtime receipts separate from these model-authored results. The
coordinator uses the runtime envelope's `execution.conversation_id`, not an ID
from this report. Verification and final review require fresh executions, with
`execution.resumed` false. Save the assigned report so the coordinator can upload
and hash it. If conversation linkage or the receipt is missing, keep the
independence limitation explicit and do not promote the affected claim. The coordinator reads reports, applies the verdict,
and retains human triage and existing finding identity.

Keep draft leads under `findings/candidates/open/`. After adjudication, confirmed
findings use `findings/open/`; unresolved ones use `needs-verification/` and
refuted ones use `dismissed/`. Preserve the existing filename, `display_id`, human
status and previous evidence. Retesting a prior finding requires its original
proof and controls. Source-only or speculative evidence remains unresolved.

Published findings use this frontmatter:

```yaml
---
title: Short finding title
finding_type: authentication-bypass
severity: high
affects:
  - asset_type: endpoint
    locator: GET /example
---
```

Use `critical`, `high`, `medium`, `low`, or `info` for severity. Keep
`finding_type` within 64 characters and `affects` nonempty. The backend assigns
finding IDs. Use these five nonempty body headings in order:

```markdown
## Attack Flow
## Description
## Evidence
## Impact
## Remediation
```

Attack Flow holds a Mermaid diagram. Description holds what the vulnerability
is. Evidence holds the redacted request and the raw captured response in a
fence. Paste the response bytes that show the leak. Do not paraphrase them.
Impact holds the business and technical consequence. Remediation holds the
numbered fix steps.
