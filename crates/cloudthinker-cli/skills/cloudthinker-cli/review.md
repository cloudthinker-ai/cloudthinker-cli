# Inspect a tracked code review

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
