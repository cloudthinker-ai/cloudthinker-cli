# Identity and authentication

Read this module when logging in, choosing a host or workspace, or recovering from an authentication error.

## Establish identity

```bash
cloudthinker whoami
cloudthinker whoami --json
cloudthinker --workspace '<workspace-id-or-name>' whoami
```

Check the returned host, account, workspace name, and workspace ID against the task. Keep the selected `--workspace` on every subsequent authenticated command. Duplicate workspace names require the ID. Missing workspace credentials fail instead of falling back to another workspace.

`whoami --json` writes one machine-readable object containing `host`, `user_id`, and `workspace_id` for integrations that must compare the CLI identity with another authenticated surface. It does not print the access token.

The default host is `https://app.cloudthinker.io`. Use `--url '<origin>'` only when the task targets another host; pass the bare origin without `/api/v1`, and keep it consistent. `CLOUDTHINKER_URL` can also select the host, so verify the resolved identity rather than assuming the default.

## Login

If authentication is missing, run the appropriate login flow when needed for the user's task:

```bash
cloudthinker login
cloudthinker login --device-auth
```

Browser login is the default. Device login supports an environment where a loopback browser callback cannot work. Relay the URL and user code that the CLI provides, and let the user complete authentication. Never request a password or access token in chat. After login, run `whoami` again.

Login selects the workspace through its consent flow. `--workspace` selects an existing stored login and cannot be combined with `login`. `CLOUDTHINKER_TOKEN` takes precedence over stored credentials and cannot be combined with `--workspace`; do not silently discard an explicitly supplied credential to switch identity.

## Recovery

Exit 3 means authentication is unavailable or rejected. Check the intended identity and login before retrying. Do not loop on rejected credentials. Use `cloudthinker login --help` for supported options.

Logout changes stored credentials. Use it only when requested or necessary for an authorized authentication repair. Plain `logout` removes the selected login; `logout --all` removes all workspace logins for that host.
