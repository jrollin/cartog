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
version, commits, tags `vX.Y.Z`, and pushes.

It also generates `editors/vscode/CHANGELOG.md` (the Marketplace renders it)
with git-cliff, using the **same config and commit range as the root
`CHANGELOG.md`**. That is deliberate: the extension is only a launcher, so the
notes users care about are the bundled binary's changes. A path-scoped run over
`editors/vscode/` would emit empty sections, because most releases touch that
directory solely via the `chore: bump version` commit that `cliff.toml` skips.
Keep the file's header bare (`# Changelog`), `--prepend` inserts the new
section right after it, so prose there would end up under the wrong version. On the tag, `release.yml` runs the
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
version, otherwise `install.sh` 404s.

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

The Marketplace authenticates through Microsoft Entra workload identity, with no
token stored ([setup below](#set-up-entra-for-the-marketplace)). Open VSX uses a
token stored as a GitHub secret (*Settings → Secrets and variables → Actions*):

| Secret | Registry | Where it comes from |
|---|---|---|
| `AZURE_CLIENT_ID` | Visual Studio Marketplace | Client ID of the managed identity |
| `AZURE_TENANT_ID` | Visual Studio Marketplace | Tenant ID of the managed identity |
| `OVSX_PAT` | Open VSX | open-vsx.org access token |

The `OVSX_PAT` is the open-vsx.org token from the manual step above.

Each registry publishes independently (`continue-on-error`), so a missing
secret or an outage on one registry does not skip the other. A failed Azure
login is tolerated the same way: the Marketplace publish then fails on its own.
The job fails only if **both** publishes fail. A same-tag re-run is therefore safe: the
registry that already has the version errors with "already published" and is
tolerated, while the other still publishes if it was behind.

### Set up Entra for the Marketplace

Azure DevOps is **retiring global PATs on 2026-12-01** ([notice][adopat]). A
Marketplace PAT spans all organizations, so it stops working on that date. CI
therefore uses Microsoft Entra ID workload-identity federation (see the
[official secure-publishing guide][vsce-entra]):

- GitHub OIDC signs in as an Azure managed identity.
- That identity is a Contributor on the Marketplace publisher.
- `vsce publish --azure-credential` uses the signed-in identity.

Do this once:

1. Create a **user-assigned managed identity** (Azure portal, *Managed
   Identities*). A free subscription is enough.
   - Do not use an App Registration or a service principal. Both sign in but
     fail at publish ([vsce #1023](https://github.com/microsoft/vscode-vsce/issues/1023)).
   - Copy the Client ID and Tenant ID from the identity's *Properties* page.
2. Opt the repo in to the immutable OIDC subject:

   ```bash
   gh api -X PUT repos/<owner>/<repo>/actions/oidc/customization/sub \
     -F use_default=true -F use_immutable_subject=true
   ```

   - Repos created before 2026-07-15 emit the name-based subject by default.
   - The Azure portal writes the ID-based subject, so without this step login
     fails with `AADSTS70021`.
3. On the identity, add a federated credential (*Federated credentials*,
   scenario *GitHub Actions deploying Azure resources*):
   - Entity type: Environment, name `marketplace-publish`.
   - Subject: `repo:<owner>@<owner-id>/<repo>@<repo-id>:environment:marketplace-publish`.
   - Not entity type Tag: it matches one exact tag, not every `v*`.
4. Create the GitHub environment `marketplace-publish` (*Settings →
   Environments*) and add `AZURE_CLIENT_ID` and `AZURE_TENANT_ID` as secrets.
   - Limit the environment to `v*` tags only after step 5, which runs from a
     branch.
5. Look up the identity's Marketplace profile id. It is not the Client ID, the
   Object ID or the resource ID. A managed identity cannot sign in from a
   laptop, so run this workflow once from a throwaway branch, copy the `id` from
   the output, then delete the branch:

   ```yaml
   on:
     push:
       branches: ["tmp/marketplace-whoami"]
   permissions: { id-token: write, contents: read }
   jobs:
     whoami:
       runs-on: ubuntu-latest
       environment: marketplace-publish
       steps:
         - uses: azure/login@v2
           with:
             client-id: ${{ secrets.AZURE_CLIENT_ID }}
             tenant-id: ${{ secrets.AZURE_TENANT_ID }}
             allow-no-subscriptions: true
         - run: az rest -u https://app.vssps.visualstudio.com/_apis/profile/profiles/me --resource 499b84ac-1321-427f-aa17-267ca6975798
   ```

6. Open the publisher's *Members* page
   (`https://marketplace.visualstudio.com/manage/publishers/<publisher>`) and
   add that id with the role Contributor.
7. After the first successful publish, delete the `VSCE_PAT` secret.

Do not rely on `vsce verify-pat --azure-credential` to check the setup: in
vsce #1023 it passed while publish still failed. Check the Marketplace listing's
version instead.

[adopat]: https://devblogs.microsoft.com/devops/retirement-of-global-personal-access-tokens-in-azure-devops/
[vsce-entra]: https://code.visualstudio.com/api/working-with-extensions/publishing-extension#secure-automated-publishing-to-visual-studio-marketplace

## After publishing

- Marketplace listing: <https://marketplace.visualstudio.com/items?itemName=jrollin.cartog>
- Open VSX listing: <https://open-vsx.org/extension/jrollin/cartog>
- Both can lag a few minutes after the job succeeds before the new version is
  searchable.
