# Remote agent

`cloudthinker remote` runs the agent on a remote machine, such as a cloud VM,
and shows it in the laptop terminal. The agent keeps running when SSH drops or
the laptop sleeps. The login, the code, and the credentials stay on the remote
machine.

Both machines need the CloudThinker CLI on Linux or macOS, and the laptop needs
plain `ssh` access to the remote machine. Run `cloudthinker whoami` on the
remote machine, not on the laptop: the agent uses the remote login.

```bash
cloudthinker remote workbox     # first time: an ssh host, user@host, user@host:port, or ssh://user@host:port
cloudthinker remote             # later: returns to the last machine
cloudthinker remote ls          # the saved machines; the first is the default
cloudthinker remote ls workbox --json   # the agents running on workbox; needs no terminal
cloudthinker remote forget workbox      # remove a saved machine
cloudthinker remote workbox --detach --cwd ~/app -- <agent args>   # start without attaching
cloudthinker remote workbox --via 'ssh -J jump me@10.0.0.5'        # reach it with another command
```

The laptop remembers the last machines. `--detach` starts the agent, prints
its name and the command that returns to it, and exits 0; it works from a
script with no terminal. `--via` runs the given command with the remote
command added at the end, instead of `ssh <host>`; the machine keeps it for
later connects; a new `--via` replaces it, and `cloudthinker remote forget`
removes the machine. The remote folder is picked for the
user: the same git repository or the same folder under home, else the one
running agent, else a short list of running agents and recent folders. In that
list, Enter continues a folder's last session, `n` starts a new one, and Esc
leaves. For a script, `--cwd <dir>` and `--name <name>` skip the list.

Inside the session:

- Ctrl+Z or `/detach` detaches at once. The agent keeps running, and
  `cloudthinker remote` returns to it.
- Ctrl+D on an empty prompt, Ctrl+C twice, `/quit`, or `/exit` asks before the
  agent stops. Enter detaches; Quit stops the agent.
- Ctrl+V pastes a laptop clipboard image. A file dragged onto the terminal is
  copied to the remote machine.
- When the link drops, the bottom row shows `Connection lost` while the laptop
  reconnects. Keys typed then are dropped, not sent late. Ctrl+C leaves, and
  the agent keeps running.

One laptop window drives an agent at a time; a second window takes it over.
`cloudthinker update` on the remote machine keeps a running agent; the next
connect uses the new version. Typing takes one SSH round trip. Windows laptops are not supported yet.

When it does not start, the CLI prints one line with the fix:

| Message | What the user does |
|---|---|
| `cloudthinker is not installed on <host>` | Answer yes in a terminal and the laptop copies its version there over SSH; the machine needs no internet. Otherwise run the printed install command on that machine |
| `cloudthinker on <host> is too old for remote` | Answer yes in a terminal; when the update there fails (no route to GitHub), the laptop copies its version instead |
| `ssh could not connect to <host>` | Fix the ssh message printed above it, then retry |
| `this machine stops the agent when you log out` | Run `loginctl enable-linger` on that machine, or ask its admin |
