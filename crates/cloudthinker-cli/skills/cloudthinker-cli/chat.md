# Delegate and follow conversations

Read this module for work sent to Anna and for recovering or continuing a conversation. Establish the intended identity using the auth module before the first authenticated command.

## Submit one bounded task

```bash
cloudthinker chat -p 'Inspect production health. Report evidence; make no changes.' --no-wait --json
```

State the target, desired outcome, constraints, and what evidence would demonstrate completion. Shell-quote the prompt safely. Do not embed secrets. Add the selected global `--workspace` and `--url` before `chat` when applicable.

Save the returned `run_id`, `conversation_id`, `status`, and `web_url`. `--no-wait` acknowledges submission, not completion. For a short task, omit `--no-wait` to wait for Anna's answer; `--json` preserves the structured output. Without `--json`, stdout carries only the final answer while progress and continuation hints go to stderr.

## Follow the existing run

```bash
cloudthinker chat status '<run-id>' --wait --timeout 60 --json
```

Choose a wait budget that fits the calling environment. Exit 4 means the client stopped waiting; the server run continues. Follow the same `run_id`, without submitting the prompt again. Without `--wait`, status reads a single snapshot, which may still be running.

Inspect `status`, `answer`, and `web_url` as well as the exit code. A successful snapshot request does not prove the task finished. A submitted run can later fail. Do not blindly retry a submission whose transport outcome is uncertain; recover the run first to avoid duplicate actions.

## Recover or continue

```bash
cloudthinker chat ls --limit 10 --json
cloudthinker chat ls --conversation '<conversation-id>' --limit 10 --json
cloudthinker chat -p 'Explain the evidence behind that conclusion.' --continue '<conversation-id>' --no-wait --json
```

Use recent runs to find an explicitly identified task, not to assume the newest row is the user's conversation. Ask for clarification if several match. `--continue` accepts a run ID or conversation ID for a HEADLESS or CHAT conversation in the selected workspace. Other feature-owned conversation types cannot be continued here.

Only one turn runs in a conversation at a time. Wait for its active run to finish before continuing. A `stream_already_active` failure means another producer still owns the conversation; follow that work before retrying.

## Outcomes

| Exit | Meaning | Next step |
| --- | --- | --- |
| 0 | Command succeeded | Inspect run status and answer before claiming completion. |
| 1 | Run or command failed | Read the failure, recover any existing run, and address the cause. |
| 2 | Invalid arguments or server validation rejection | Correct the request; do not repeat unchanged. |
| 3 | Authentication failure | Read the auth module. |
| 4 | Client wait expired | Follow the same run ID. |
| 5 | Browser approval required | Give the user the returned link and explain what is waiting. |

Do not approve actions on the user's behalf or create a replacement run to bypass approval. Once the user has acted, inspect the original run again.

Report Anna's answer with the run or conversation ID and available evidence link. Distinguish an unsupported conclusion from an outcome the answer actually demonstrates.
