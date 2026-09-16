# Wiring `cloudthinker.io/install.sh`

Handoff doc for whoever owns the **cloudthinker.io Vercel project**. Goal: make the
vanity install URLs redirect to the CLI's latest GitHub release installer, so the
published one-liner never pins a version.

```
cloudthinker.io/install.sh   ->  latest cloudthinker-cli shell installer
cloudthinker.io/install.ps1  ->  latest cloudthinker-cli PowerShell installer
```

## Where this goes

`cloudthinker.io` is served by **Vercel**: the landing-page repo
(`product/landing-page`, Next.js). The redirect lives in the `redirects()` array
of its `apps/web/next.config.mjs` — **not** in the CLI repo, and **not** in the
app monorepo (the app is `*.cloudthinker.io`). The same project also serves
`cloudthinker.ai`, so the old `.ai` install URLs keep working; `.io` is the
canonical host to publish.

## Config

The live wiring uses the Next.js form in `apps/web/next.config.mjs`:

```js
{
  source: '/install.sh',
  destination: 'https://github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.sh',
  permanent: false,
},
{
  source: '/install.ps1',
  destination: 'https://github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.ps1',
  permanent: false,
},
```

A plain-Vercel project would put the same two entries in a root `vercel.json`
instead. Either works; pick one, not both.

### Why `redirects`, not `rewrites`

`redirects` returns a 3xx and the user's `curl` follows it straight to GitHub —
Vercel serves nothing and uses no bandwidth. `rewrites` would proxy the installer
through Vercel (extra hop, Vercel bandwidth, GitHub rate-limit exposure). Use
`redirects`.

`permanent: false` = HTTP 307 (temporary). Keep it temporary so browsers/CDNs don't
cache the hop — the release the URL points at changes over time even though the
redirect itself is stable. `curl -L` follows 307/308 for GET either way.

## Prerequisite: the target must exist

Until both are true, the GitHub URL 404s (and so will the vanity URL):

1. The releases repo `cloudthinker-ai/cloudthinker-cli` is **public** (unauthenticated
   `curl` must read release assets). The source lives in the private
   `cloudthinker-ai/cloudthinker-cli-src`, whose release workflow publishes there.
2. At least **one release exists** (a pushed `vX.Y.Z` tag on the source repo ran the
   release workflow), so `/releases/latest/` resolves.

You can add the `vercel.json` redirect before those are done — it will simply 404 at
the GitHub hop until the first public release lands, then start working with no
further change.

## Verify

After a public release exists:

```sh
# should print a 307 to github.com, then (with -L) the installer script
curl -sSI https://cloudthinker.io/install.sh | grep -i '^location'
curl -fsSL https://cloudthinker.io/install.sh | head -5
```

The published end-user one-liner (docs, landing page, release repo README):

```sh
curl -fsSL https://cloudthinker.io/install.sh | sh
```

Keep `/install.ps1` wired, but do not publish a Windows one-liner yet: the CLI
ships a Windows binary while the `cloudthinker-agent` bundle does not, so the
agent command fails there. The redirect is ready for the day that changes.
