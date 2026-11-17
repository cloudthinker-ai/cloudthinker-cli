/**
 * tool-description.ts — the model-facing description of the `ct_workflow` tool.
 *
 * This is a deliberate port of Claude Code's `Workflow` tool description, not a
 * paraphrase of it. The rule the text is held to: **match Claude Code's wording
 * everywhere; deviate only in the specific clause where its sentence would be
 * false about pi, and keep that deviation minimal and in its voice.** Wording
 * parity is the point — a user who knows one tool should not have to relearn
 * the other, and the orchestration patterns below are load-bearing guidance
 * that gets used badly when compressed.
 *
 * Parts omitted because pi has no such feature: the `ultracode` opt-in, MCP
 * tools reached through `ToolSearch`, the `agent-<id>.jsonl` resume fallback,
 * and the `/config` workflow-size guideline.
 *
 * Clauses that had to deviate, each because Claude Code's is untrue here:
 *   - `schema` is pressure, not force — `toolChoice` is not plumbed through
 *     pi's `AgentSession`, so a child can decline and the call returns null.
 *   - `budget.total` is always null; pi has no token-target directive.
 *   - `parallel` propagates a fatal run error instead of folding it to null.
 *   - `effort` inherits the agent definition's level, then the parent's.
 *   - `isolation` removes the worktree on settle, changes kept on a branch.
 * Additions with no upstream counterpart: `gate`, `resume`, `effort: "minimal"`,
 * the saved-workflow directories, and the reject-unknown-options guarantee.
 *
 * Kept out of index.ts purely for size. `{{placeholder}}` tokens are rendered by
 * the same substitution pass the Agent tool's description uses, so a
 * user-authored override can interpolate the live agent roster.
 */

/**
 * Rendered with `{{typeList}}` substituted. Keep the prose accurate to what the
 * runtime actually implements — documenting a global we do not ship is worse
 * than documenting nothing, because the script only fails once it is running.
 * `workflow-tool-description.test.ts` pins the parts that can drift: the
 * `agent()` option set, the `resume` exclusions, the effort levels, the caps,
 * and that every example here uses options the runtime actually accepts.
 */
export const fullWorkflowToolDescription = `Execute a workflow script that orchestrates multiple subagents deterministically. Workflows run in the background — this tool returns immediately with a task ID, and you are notified when the workflow completes, so do not poll or sleep. Use /agents → Workflows to watch live progress.

A workflow structures work across many agents — to be comprehensive (decompose and cover in parallel), to be confident (independent perspectives and adversarial checks before committing), or to take on scale one context can't hold (migrations, audits, broad sweeps). The script is where you encode that structure: what fans out, what verifies, what synthesizes.

Use this tool when the user's plain-language task needs multiple stages, work that depends on discoveries, or independent verification. The request for the work authorizes this orchestration; the user does not need to name workflows or subagents. Keep the fan-out proportional to the task and use the workflow progress display to show what is running.

When you do call it, the right move is often **hybrid**: scout inline first (list the files, find the channels, scope the diff) to discover the work-list, then call ct_workflow to pipeline over it. You don't need to know the shape before the *task* — only before the *orchestration step*.

For larger work, chain single-phase workflows across turns (understand, design, review, research, migrate) and read each result before deciding the next phase.

Pass the script inline via \`script\` — do not Write it to a file first. Every invocation automatically persists its script to a file under the session directory and returns the path in the tool result. To iterate on a workflow, edit that file with Write/Edit and re-invoke ct_workflow with \`{scriptPath: "<path>"}\` instead of resending the full script. A script you will run more than once belongs in \`.pi/workflows/<name>.js\` (or \`.agents/workflows/\`, or \`<agent dir>/workflows/\` for one that follows the user everywhere); call it with \`name: "<name>"\` instead of re-sending the source.

Every script must begin with \`export const meta = {...}\`:
  export const meta = {
    name: 'find-flaky-tests',
    description: 'Find flaky tests and propose fixes',   // one-line, shown in permission dialog
    phases: [                                            // one entry per phase() call
      { title: 'Scan', detail: 'grep test logs for retries' },
      { title: 'Fix', detail: 'one agent per flaky test' },
    ],
  }
  // script body starts here — use agent()/parallel()/pipeline()/phase()/log()
  phase('Scan')
  const flaky = await agent('grep CI logs for retry markers', {schema: FLAKY_SCHEMA})
  ...

The \`meta\` object must be a PURE LITERAL — no variables, function calls, spreads, or template interpolation. Required fields: \`name\`, \`description\`. Optional: \`whenToUse\` (shown in the workflow list), \`phases\`. Use the SAME phase titles in meta.phases as in phase() calls — titles are matched exactly; a phase() call with no matching meta entry just gets its own progress group. Add \`model\` to a phase entry when that phase uses a specific model override.

Script body hooks:
- agent(prompt: string, opts?: {label?: string, phase?: string, schema?: object, receipt?: boolean, model?: string, effort?: string, isolation?: 'worktree', agentType?: string, gate?: string, resume?: string}): Promise<any> — spawn a subagent. Without schema, returns its final text as a string. With schema (a JSON Schema), the subagent is given a StructuredOutput tool built from it and agent() returns the validated object — no parsing needed. A payload that does not match is rejected back to the child, which corrects it; a child that never answers through the tool fails, so the call returns null. Returns null as well if the user skips the agent or it dies on a terminal API error, so filter with .filter(Boolean) after every schema stage. opts.receipt: true returns {result, execution: {record_id, conversation_id?, resumed}} instead of the bare answer; the runtime preserves execution fields on journal replay, and a resumed call keeps its execution and is not independent. opts.label overrides the display label. opts.phase assigns the agent to a progress group (use it inside pipeline()/parallel() stages; same string, same group). opts.model overrides the model for this call; omit it unless you are highly confident a different tier fits, because the agent otherwise inherits the session model. opts.effort overrides the reasoning effort for this agent call ('minimal' | 'low' | 'medium' | 'high' | 'xhigh' | 'max') — omit to inherit the agent definition's own level, then the parent's; use 'low' for cheap mechanical stages and higher tiers only for the hardest verify/judge stages. opts.isolation: 'worktree' runs the agent in a fresh git worktree — EXPENSIVE, use ONLY when agents mutate files in parallel and would otherwise conflict; the worktree is removed when the agent settles and its changes stay on a branch. opts.gate: '<command>' runs a shell command after the agent finishes and requires it to pass — a non-zero exit marks the agent failed and the command's output becomes the error; prefer gate: 'npm test' over asking another agent whether the code looks right. opts.resume: '<label>' continues the child that ran under that label instead of starting fresh, so an iterative loop keeps its context — it cannot be combined with agentType, model, effort, isolation, gate or schema. opts.agentType uses a custom subagent type instead of the default workflow subagent — resolved from the same registry as the Agent tool; composes with schema. Available types:
{{typeList}}
- pipeline(items, stage1, stage2, ...): Promise<any[]> — run each item through all stages independently, NO barrier between stages. Item A can be in stage 3 while item B is still in stage 1. This is the DEFAULT for multi-stage work. Wall-clock = slowest single-item chain, not sum-of-slowest-per-stage. Every stage callback receives (prevResult, originalItem, index) — use originalItem/index in later stages to label work without threading context through stage 1's return value. A stage that throws drops that item to \`null\` and skips its remaining stages.
- parallel(thunks: Array<() => Promise<any>>): Promise<any[]> — run tasks concurrently. This is a BARRIER: awaits all thunks before returning. A thunk that throws (or whose agent errors) resolves to \`null\` in the result array, so \`.filter(Boolean)\` before using the results; only a fatal run error — a cap breach, or a nested workflow that could not load — propagates instead of being folded into a null. Use ONLY when you genuinely need all results together.
- log(message: string): void — emit a progress message to the user (shown as a narrator line above the progress tree)
- phase(title: string): void — start a new phase; subsequent agent() calls are grouped under this title in the progress display
- args: any — the value passed as ct_workflow's \`args\` input, verbatim (undefined if not provided). Pass arrays/objects as actual JSON values in the tool call, NOT as a JSON-encoded string — \`args: ["a.ts", "b.ts"]\`, not \`args: "[\\"a.ts\\", ...]"\` (a stringified list reaches the script as one string, so \`args.filter\`/\`args.map\` throw). Use this to parameterize named workflows — e.g. pass a research question, target path, or config object directly instead of via a side-channel file.
- budget: {total: number|null, spent(): number, remaining(): number} — \`budget.total\` is always null here: it comes from a token-target directive pi does not have, so guards like \`while (budget.total && budget.remaining() > 50_000) { ... }\` correctly do not fire rather than throwing on a missing global. \`budget.spent()\` returns output tokens spent by this run's agents. \`budget.remaining()\` returns \`Infinity\` with no target.
- workflow(nameOrRef: string | {scriptPath: string}, args?: any): Promise<any> — run another workflow inline as a sub-step and return whatever it returns. Pass a name to invoke a saved workflow (same registry as {name: "..."}), or {scriptPath} to run a script file you Wrote earlier. The child shares this run's concurrency cap, agent counter, abort signal, and token budget — its agents appear under a "▸ name" group in /agents → Workflows and its tokens count toward budget.spent(). The args param becomes the child's \`args\` global. Nesting is one level only: workflow() inside a child throws. Throws on unknown name / unreadable scriptPath / child syntax error; catch to handle gracefully.

Any agent() option not listed above is rejected by name at the call.

Subagents are told their final text IS the return value, so they return raw data; use the schema option for structured output.

Scripts are plain JavaScript, NOT TypeScript — type annotations (\`: string[]\`), interfaces, and generics fail to parse. The script body runs in an async context — use await directly. Standard JS built-ins (JSON, Math, Array, etc.) are available — EXCEPT \`Date.now()\`/\`Math.random()\`/argless \`new Date()\`, which throw (they would break resume); pass timestamps in via \`args\`, stamp results after the workflow returns, and for randomness vary the agent prompt/label by index. \`eval\` and \`Function(...)\` throw. No filesystem or Node.js API access.

DEFAULT TO pipeline(); each item flows through all stages with no barrier, so wall-clock is the slowest chain, not the sum of the slowest stages. Use parallel() between stages only when stage N needs cross-item context from all of stage N-1 (dedup across the full set, early exit on zero results, comparing against "the other findings"). Flattening, mapping, filtering or "cleaner code" do not justify a barrier: put the transform inside a pipeline stage.

Concurrent agent() calls are capped at the configured session limit; excess calls queue. Nested workflows share this limit.

The canonical multi-stage pattern — pipeline by default, each dimension verifies as soon as its review completes:
  export const meta = {
    name: 'review-changes',
    description: 'Review changed files across dimensions, verify each finding',
    phases: [{ title: 'Review' }, { title: 'Verify' }],
  }
  const DIMENSIONS = [{key: 'bugs', prompt: '...'}, {key: 'perf', prompt: '...'}]
  const results = await pipeline(
    DIMENSIONS,
    d => agent(d.prompt, {label: \`review:\${d.key}\`, phase: 'Review', schema: FINDINGS_SCHEMA}),
    review => parallel(review.findings.map(f => () =>
      agent(\`Adversarially verify: \${f.title}\`, {label: \`verify:\${f.file}\`, phase: 'Verify', schema: VERDICT_SCHEMA})
        .then(v => ({...f, verdict: v}))
    ))
  )
  const confirmed = results.flat().filter(Boolean).filter(f => f.verdict?.isReal)
  return { confirmed }
  // Dimension 'bugs' findings verify while dimension 'perf' is still reviewing. No wasted wall-clock.

Gate-and-retry pattern — verify by running, and keep the agent's context across attempts:
  let fixed = await agent('Find and fix the failing test.', {label: 'fix', gate: 'npm test'})
  if (fixed === null) {                        // a non-zero exit failed the agent
    // Resume keeps everything the child already learned. It cannot carry the
    // gate, so re-verification needs its own gated call, in the same tree.
    fixed = await agent('\`npm test\` is still failing. Fix the cause.', {label: 'fix', resume: 'fix'})
    const verified = await agent('Run \`npm test\` and report the result. Change nothing.',
      {label: 'verify', phase: 'Verify', gate: 'npm test', effort: 'low'})
    return { passed: verified !== null, summary: fixed }
  }
  return { passed: true, summary: fixed }
  // An LLM judging whether a fix works is a weaker signal than the test suite.

Quality patterns, composed freely: adversarial verify (N skeptics told to refute; kill on majority refute), verify by running (gate) rather than asking, perspective-diverse verifiers (a distinct lens each), judge panel (N independent angles scored, synthesized from the winner), loop-until-dry (keep finding until K rounds return nothing new, dedup against all seen, not only confirmed), multi-modal sweep, completeness critic, and no silent caps (log() whatever coverage is dropped).

Scale to what the user asked for. "find any bugs" → a few finders, single-vote verify. "thoroughly audit this" or "be comprehensive" → larger finder pool, 3–5 vote adversarial pass, synthesis stage. When unsure, lean toward thoroughness for research/review/audit requests and toward brevity for quick checks.

Use this tool for multi-step orchestration where control flow should be deterministic (loops, conditionals, fan-out) rather than model-driven.

## Resume

The tool result includes a runId. To resume after a pause, kill, or script edit, relaunch with ct_workflow({scriptPath, resumeFromRunId}) — the longest unchanged prefix of agent() calls returns cached results instantly; the first edited/new call and everything after it runs live. Same script + same args → 100% cache hit. It is a prefix and not a lookup: a later call that still matches is not reused once an earlier one has changed. A journaled failure ends the prefix, so resuming a run that died at agent 5 retries exactly agent 5. Same session only. A matching active run returns its existing task; completed runs can be resumed from their journal. Before diagnosing why a completed workflow returned an empty or unexpected result, Read the run's \`<run id>.workflow.jsonl\` beside its script — it records each agent's actual return value; do not assume cached results are non-empty. Date.now()/Math.random()/new Date() are unavailable in scripts (they would break this) — stamp results after the workflow returns, or pass timestamps via args.`;
