# Review local changes or inspect a tracked review

## Review the current worktree locally

```bash
cloudthinker review
cloudthinker review --base origin/develop --json
```

Bare `review` checks staged, unstaged, and non-ignored untracked changes against `HEAD`. `--base`
checks from the merge base with that ref and includes dirty edits. This path requires CloudThinker
login and starts the bundled Pi agent in the local checkout with read-only `read`, `grep`, `find`,
and `ls` tools. CloudThinker supplies model inference, so requests and responses pass through its normal gateway observability. Local Pi entries and findings are not mirrored as Agent CLI session entries or saved chat messages. Findings are validated and printed by the CLI. It does not create
a source-control review, post comments, edit files, or change the Git index. A clean result means
the model returned no findings; it is not a guarantee that the change is defect-free.

When the scope is empty, the base cannot resolve, authentication fails, CloudThinker is unavailable,
or the model response is invalid, report the error as a failed review. Do not turn an incomplete
review into a clean result. A timeout stops the local agent; rerun the review to try again.

## Inspect a tracked review

Read this module when checking review progress or findings for a merge request or pull request. Establish the intended identity using the auth module before the first authenticated command.

## Read status and findings

```bash
cloudthinker review status '<mr-or-pr-url>' --json
cloudthinker review findings '<mr-or-pr-url>' --json
```

Use the exact provider URL from the task. Add the selected global `--workspace` and `--url` before `review` when applicable. These commands inspect an already tracked CloudThinker review. They do not trigger a review, submit fixes, resolve findings, approve, or merge the provider request.

Read the returned status, verdict, severity counts, and findings. Exit 0 means the lookup succeeded; it does not mean the review passed or has no findings. Report the actual review result and any incomplete coverage.

## Wait for the review

```bash
cloudthinker review watch '<mr-or-pr-url>' --timeout 60 --json
```

A client timeout stops waiting without stopping the review. Recheck the same URL. If the review is unknown, verify the URL and workspace; do not invent a trigger command or claim there are zero findings.

For authentication errors, read the auth module. For unsupported operations, state the limitation and use another authorized workflow only when the task calls for it.
