# Wiring `cloudthinker.ai/install.sh`

Handoff doc for whoever owns the **cloudthinker.ai Vercel project**. Goal: make the
vanity install URLs redirect to the CLI's latest GitHub release installer, so the
published one-liner never pins a version.

```
cloudthinker.ai/install.sh   ->  latest cloudthinker-cli shell installer
cloudthinker.ai/install.ps1  ->  latest cloudthinker-cli PowerShell installer
```

## Where this goes

`cloudthinker.ai` is served by **Vercel** (confirmed via response headers). The
redirect lives in the **`vercel.json`** at the root of the Vercel project that serves
that domain — **not** in the CLI repo, and **not** in the app monorepo (the app is
`*.cloudthinker.io`).

## Config

Add to `vercel.json`:

```json
{
  "redirects": [
    {
      "source": "/install.sh",
      "destination": "https://github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.sh",
      "permanent": false
    },
    {
      "source": "/install.ps1",
      "destination": "https://github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.ps1",
      "permanent": false
    }
  ]
}
```

If the project is Next.js you can instead add the same two entries to
`next.config.js` under `async redirects()`. Either works; pick one, not both.

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
curl -sSI https://cloudthinker.ai/install.sh | grep -i '^location'
curl -fsSL https://cloudthinker.ai/install.sh | head -5
```

End-user install one-liners (for reference / docs / landing page):

```sh
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf https://cloudthinker.ai/install.sh | sh
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://cloudthinker.ai/install.ps1 | iex"
```
