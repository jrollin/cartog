# Publish the VS Code extension

Maintainer runbook for shipping the cartog VS Code extension to the **Visual
Studio Marketplace** (stock VS Code) and **Open VSX** (Cursor, Windsurf,
VSCodium, Gitpod).

This mirrors how cartog publishes to crates.io: the publish runs in CI on a
`v*` tag, gated by repository secrets. Routine releases need **no manual
steps**: the one-time account/token setup below is done once, then
`./scripts/release.sh` does the rest.

The extension lives in [`editors/vscode/`](../../editors/vscode/). It ships no
binary: it locates the user's installed `cartog` and registers
`cartog serve --watch` as an MCP server for Copilot. So the `.vsix` is tiny,
platform-agnostic, and independent of the release build matrix.

## How a release happens (routine)

```bash
./scripts/release.sh minor      # bumps Cargo.toml, plugin.json, the install
                                # skill, the site, AND editors/vscode/package.json
```

The script keeps `editors/vscode/package.json` in lockstep with the workspace
version, commits, tags `vX.Y.Z`, and pushes. On the tag, `release.yml` runs the
`publish-vscode` job, which:

1. installs Node deps and compiles the extension (`vscode:prepublish` → `tsc`),
2. **verifies** `package.json` version equals the tag (fails the job otherwise),
3. packages `cartog.vsix` with `vsce package`,
4. signs in to Entra (`azure/login`, OIDC) and publishes to the Visual Studio
   Marketplace (`vsce publish --azure-credential`),
5. publishes to Open VSX (`ovsx publish`, reads `OVSX_PAT`),
6. uploads the `.vsix` as a build artifact.

The extension publish and the crates.io publish are **independent jobs** in the
same workflow, so one failing does not roll back the other. Both are gated on
the `release` job (see below), so neither can advertise a version before its
GitHub Release exists.

### Version pinning and release ordering

The extension's in-editor **Install cartog** action pins `CARTOG_VERSION` to the
extension's own version, so the installed binary matches the extension. The
installer fetches `releases/download/v<version>/`, so that **GitHub Release and
its binary assets must exist before a user runs Install** for the just-published
version — otherwise `install.sh` 404s.

`publish-vscode` therefore declares `needs: release`, and `release` in turn
needs the whole build matrix. The tag's binary assets are published before the
Marketplace serves the matching extension version, so there is no window in
which Install resolves a version whose tarballs are missing.

One narrow gap remains by design: `softprops/action-gh-release` makes the
release public as it uploads, so for the seconds during which the last assets
land, `releases/latest` already resolves to the new tag. A `cartog self update`
that starts inside that window can 404 on its platform tarball or on
`SHA256SUMS`; re-running it succeeds. This does not affect the extension path,
which never consults `releases/latest`.

## Publish from your machine first

Do a hand publish once before wiring CI. It validates the package and the
publisher accounts without needing the OIDC plumbing the workflow uses.

### 1. Build and smoke-test the `.vsix`

```bash
cd editors/vscode
npm install
npm run compile
npx @vscode/vsce package --no-dependencies -o cartog.vsix
code --install-extension cartog.vsix      # sideload into VS Code
```

Open a project, open Copilot Chat, and confirm the cartog tools appear (the MCP
server shows as `cartog` and starts with no config file). Check the
missing-binary path too: rename `cartog` off `PATH` and reload, you should get
the install prompt, not a silent failure.

### 2. Publish to the Visual Studio Marketplace

A one-time publisher is required at
<https://marketplace.visualstudio.com/manage>; its ID must match `"publisher"`
in `editors/vscode/package.json` (currently `jrollin`), so the extension is
`jrollin.cartog`.

```bash
npx @vscode/vsce login jrollin     # interactive: paste a Marketplace credential at the prompt
npx @vscode/vsce publish --no-dependencies --packagePath cartog.vsix
```

### 3. Publish to Open VSX

Open VSX is the registry VS Code forks (Cursor, Windsurf, VSCodium, Gitpod)
read from. One-time: sign in at <https://open-vsx.org>, accept the publisher
agreement, create a token, and claim the namespace
(`npx ovsx create-namespace jrollin -p <token>`).

```bash
OVSX_PAT=<token> npx ovsx publish cartog.vsix
```

Both registries reject re-publishing an already-published version, so bump
`package.json` first (or let `release.sh` do it).

## Wire CI (after the manual publish works)

Both registries authenticate with a token stored as a GitHub repo secret
(*Settings → Secrets and variables → Actions*):

| Secret | Registry | Where it comes from |
|---|---|---|
| `VSCE_PAT` | Visual Studio Marketplace | Azure DevOps PAT, scope `Marketplace → Manage`, organization `All accessible organizations` |
| `OVSX_PAT` | Open VSX | open-vsx.org access token |

The `VSCE_PAT` is the same kind of Marketplace token you paste at the
`vsce login` prompt in the manual step above. The `OVSX_PAT` is the open-vsx.org
token from that step.

Each registry publishes independently (`continue-on-error`), so a missing
secret or an outage on one registry does not skip the other. The job fails
only if **both** publishes fail. A same-tag re-run is therefore safe: the
registry that already has the version errors with "already published" and is
tolerated, while the other still publishes if it was behind.

### Migrate off the PAT before 2026-12-01

Azure DevOps is **retiring global PATs on 2026-12-01** ([notice][adopat]). A
Marketplace PAT spans all organizations, so it is a global PAT and stops working
on that date. The successor is Microsoft Entra ID workload-identity federation:
`vsce publish --azure-credential` authenticated by `azure/login` (OIDC), with no
token secret. See the [official secure-publishing guide][vsce-entra]. Migrate
the `publish-vscode` job before the cutoff.

[adopat]: https://devblogs.microsoft.com/devops/retirement-of-global-personal-access-tokens-in-azure-devops/
[vsce-entra]: https://code.visualstudio.com/api/working-with-extensions/publishing-extension#secure-automated-publishing-to-visual-studio-marketplace

## After publishing

- Marketplace listing: <https://marketplace.visualstudio.com/items?itemName=jrollin.cartog>
- Open VSX listing: <https://open-vsx.org/extension/jrollin/cartog>
- Both can lag a few minutes after the job succeeds before the new version is
  searchable.
