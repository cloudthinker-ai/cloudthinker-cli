# Shared Cyber investigation contract

Cloud and local Runs follow the same investigation lifecycle. The execution
adapter supplies commands, paths, identity references and worker admission.
Read that adapter before executing work. This contract supplies methodology;
it grants no target access, credentials, tools or additional scope.

## Prepare and reuse

Read the frozen target, scope, probing policy, framework, intensity, identities,
report preferences and saved Run state. Reuse discovery, useful negative results,
coverage, candidate evidence and checkpoint handoffs. Prior findings are context;
only a fresh retest of their original proof can change their current outcome.
An absent artifact or failed collector is unknown, never a clean result.

Before assigning authenticated work, prove protected access for each required
identity and record its safe selector, revision, purpose, known expiry and last
observed health. A configured variable or stored session alone proves no login.
Keep operational sessions separate from logout, replay, expiry and revocation
experiments. Coordinate shared session changes and prepare recovery first.
Never silently replace an unavailable authenticated identity with anonymous work.
Continue independent authorized work while a prerequisite is blocked.

## Coordinate one investigation

The coordinator is the agent that owns this investigation. In Fortress, each
persistent teammate coordinates its own investigation and bounded workers.
It joins independent proof and saves supported findings and attack surface
without Manager approval. The Manager owns shared synthesis and the final Run
report; bounded workers return evidence to their coordinating investigator.
The coordinator owns priorities, assignments, collector reconciliation, evidence
synthesis, finding adjudication, checkpoints and investigation summaries. Workers own target
interaction and source investigation. A thin surface can use smaller assignments;
a confirmed finding still needs independent technical verification.

1. **Recon:** run deterministic discovery alongside semantic mapping of routes,
   architecture, trust boundaries, identities and authenticated business journeys.
   Preserve provenance and unresolved questions. Reconcile the results into saved
   surface before issuing focused work. Recon returns observations, not findings.
   Record observed stack components, version evidence and uncertainty. Research
   public advisories when components appear, including when versions are unknown.
2. **Plan:** compare known surface with source, API contracts and expected business
   rules. Prioritize reachable impact and testable hypotheses. A Critic reviews
   missing identities, workflows, prerequisites and weak assumptions. The
   coordinator accepts, rejects or defers each gap with its reason.
3. **Hunt:** assign bounded questions and coverage rows. Start with an observation,
   the rule being tested, the suspected failure and a discriminating control.
   Let observed behavior guide follow-up and connected attack paths. Preserve
   candidate evidence and useful negative observations.
   Check relevant advisories against deployed versions, configuration and reachable
   functionality. A version match alone is a lead, not verified exploitation.
4. **Verify:** send ready candidates to fresh independent worker executions while
   unrelated hunting continues. Group claims that share replay setup, but obtain
   a separate observed effect, control and verdict for every claim.
5. **Join:** read required worker reports, reconcile newly discovered surface,
   adjudicate candidates and account for remaining coverage. Critic feedback can
   produce another Recon, Hunt or Verify assignment. Join required work before
   checkpointing; a pass boundary does not restart completed discovery.
6. **Checkpoint:** save evidence and a compact handoff, then use the execution
   adapter's server-owned continue-or-close decision. Continue within the same
   Run, frozen authority and remaining budget. A close decision and report
   readiness are separate facts. Repair returned gaps before publication.

Assign each worker its question, target/path/method bounds, identity selectors
and revisions, relevant saved context, output location and expected result shape.
A child does not automatically receive the coordinator's conversation history.
Use distinct output paths and coordinate mutable identities and prerequisites.
A worker failure remains failed or blocked until repaired; never infer its result.

## Prove claims and preserve outcomes

Technical verification must independently replay the exact claim and its controls.
For access boundaries, compare the same resource or operation across the relevant
identities and an allowed control. Status codes alone do not establish a flaw.
For state changes, inspect the resulting state. Prove every prerequisite and the
combined effect before claiming a multi-step chain. Source, scanner output,
unexpected responses and producer prose remain candidate evidence.

Return confirmed, refuted or inconclusive per claim. Confirmation needs an
observed unsafe effect, expected rule, controls, preconditions and supported
impact. Refutation needs a complete replay and controls that disprove the claim.
Missing access, state or evidence leaves the claim inconclusive with the blocker.
Only the coordinator adjudicates joined results. Preserve human triage, finding
identity and earlier evidence when retesting or correcting an outcome.

Technical verification uses an execution distinct from all producers. A
high-severity or contested final record also receives a separate final-record
review, distinct from producers and technical verifiers. That review checks the
exact adjudicated revision without modifying it. A material change to the effect,
asset, identity preconditions, impact or evidence requires fresh technical proof.
A role label, invented UUID or copied report cannot establish independence.
Use actual runtime-issued execution references and their saved evidence. Missing
server-verifiable admission remains an explicit independence limitation.

## Record coverage honestly

Extend the existing inventory when new surface appears. Preserve completed
outcomes and evidence references when refreshing a plan. Record assessed work,
candidates, blocked prerequisites and skipped work with reasons through the
adapter's coverage authority. An untested or assigned row remains unfinished.
Account for component research as researched, deferred or blocked with its
advisory sources, research date and missing applicability evidence. Public
research sends only public product/version/advisory terms, never target details,
source, private package names or credentials. Use the execution adapter's
research tools; keep isolated scanners offline and record unavailable research.
A terminal ledger accounts for known inventory; it does not prove whole-app
coverage or that every applicable security check was selected.

Evidence includes redacted requests, captured responses or state effects,
controls, source references where relevant, and the identity/revision used.
A failed probe is not negative security evidence. Preserve failed attempts and
limitations without promoting them into covered or confirmed outcomes.

## Close and report

Join required workers and preserve their reports. Ultra also needs a closure
Critic review of current evidence. A ready validator result never substitutes
for that review or proves exhaustive coverage. Revalidate changed records before
publishing the final report. Report confirmed findings, unresolved claims,
tested versus blocked and skipped scope, discovery limits, prior findings not
retested, unresolved advisory matches and stack blind spots, and the actual stop
reason. Retain useful questions in existing App knowledge and refresh advisory
and deployment evidence on later Runs. A blocked or failed investigation produces
honest partial results. Never widen authority or start another Run to evade limits.
