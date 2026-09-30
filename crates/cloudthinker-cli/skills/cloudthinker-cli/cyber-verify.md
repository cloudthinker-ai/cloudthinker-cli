# Local Cyber verification

Use an independent verifier for each candidate when the workflow can dispatch
one. The verifier must replay the claim; the scout's original response is not
verification. Read the candidate, relevant source or contract, and expected
access rules. Test against the run's frozen target and record a fresh observation.
Give the verifier the claim, policy, and raw evidence; let it decide the outcome.

For authorization claims, compare the same resource across relevant identities,
including an allowed control. Status codes alone do not prove a boundary
failure. For state changes, inspect the effect and use a safe control. Confirm
only when the unsafe effect reproduces and evidence supports impact. Refute only
after a complete replay and controls show the claim does not hold. Otherwise,
leave it unresolved and record the missing identity, state, control, or other
blocker; do not promote source-only or speculative evidence.
Record a contract or usability mismatch as an observation unless it violates a
security property; an unexpected response alone is not a vulnerability.
Move prior false positives to `findings/dismissed/`, retaining proof and the
reason for the new verdict.
Keep an existing finding's filename, `display_id`, and human status metadata.

Keep the redacted request, observed result, control, and source references with
the candidate evidence. Put confirmed findings under `findings/open/`; use
`needs-verification/` or `dismissed/` for other outcomes as appropriate.

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
