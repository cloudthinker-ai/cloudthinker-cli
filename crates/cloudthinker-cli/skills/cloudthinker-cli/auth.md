# Identity and authentication

Read this module when logging in, choosing a host or workspace, or recovering from an authentication error.

## Establish identity

```bash
cloudthinker whoami
cloudthinker whoami --json
cloudthinker --workspace '<workspace-id-or-name>' whoami
```

Check the returned host, account, workspace name, and workspace ID against the task. Keep the selected `--workspace` on every subsequent authenticated command. Duplicate workspace names require the ID. Missing workspace credentials fail instead of falling back to another workspace.

`whoami --json` writes one machine-readable object containing `host`, `user_id`, `user_email`, `workspace_id`, and `workspace_name` for integrations that must compare the CLI identity with another authenticated surface. It does not print the access token.

The host resolves as `--url`, then `CLOUDTHINKER_URL`, then the origin of the user's last `login --url`, then `https://app.cloudthinker.io`. Use `--url '<origin>'` only when the task targets another host; pass the bare origin without `/api/v1`, and keep it consistent. Verify the resolved identity rather than assuming the default.

## Login

If authentication is missing, run the appropriate login flow when needed for the user's task:

```bash
cloudthinker login
cloudthinker login --device-auth
```

Browser login is the default on a desktop. Over SSH or on Linux without a display, `login` uses device login itself, because a loopback browser callback cannot reach the machine. `CLOUDTHINKER_LOGIN=browser` or `device` overrides that choice. `login --url` makes that origin the default for later commands, so use it only when the user asks to switch hosts. Relay the URL and user code that the CLI provides, and let the user complete authentication. Never request a password or access token in chat. After login, run `whoami` again.

Login selects the workspace through its consent flow. `--workspace` selects an existing stored login and cannot be combined with `login`. `CLOUDTHINKER_TOKEN` takes precedence over stored credentials and cannot be combined with `--workspace`; do not silently discard an explicitly supplied credential to switch identity.

## Stored logins

```bash
cloudthinker auth status
cloudthinker auth status --json
cloudthinker auth switch '<workspace-id-or-name>'
```

`auth status` lists the stored workspace logins for the host and marks the active one without a network call or a token. `auth switch` changes the active login; prefer `--workspace` for a single command.

## Recovery

Exit 3 means authentication is unavailable or rejected. Check the intended identity and login before retrying. Do not loop on rejected credentials. Use `cloudthinker login --help` for supported options. Run the login command that the error names: it carries `--url` for a non-default host.

An `API error 403` also exits 3, but a new login does not fix it: the account lacks access to that workspace or feature. Report the server's reason to the user.

Logout changes stored credentials. Use it only when requested or necessary for an authorized authentication repair. Plain `logout` removes the selected login; `logout --all` removes all workspace logins for that host.
