# Gitee release mirror

`Sync releases to Gitee` copies published stable releases from GitHub
`TortoTech/torto` to Gitee `TortoTech/torto`. It runs after successful Windows or
macOS tag builds and on published/edited release events. Ordinary branch builds
have no eligible release and are skipped. The workflow must be on GitHub `main`
before automatic triggers or manual dispatch become available.

Configure GitHub Actions repository secret `GITEE_TOKEN` with a Gitee personal
token that can read/write the destination repository. No token is shipped in
Torto. The workflow uses GitHub's built-in read-only token for source metadata.

Before any Gitee writes, the script requires the MSI, both macOS DMGs and their
SHA-256 files. Automatic runs skip incomplete releases; a later successful build
triggers another check. Manual runs fail if attachments are incomplete.

The exact GitHub tag is pushed without force (an existing tag must reference the
same commit). Release title and body are copied without rewriting their links.
All GitHub release attachments are mirrored, including any additional files.
Existing matching attachments are reused and verified by size and SHA-256;
missing ones are uploaded. Three attempts reconcile remote state after failures,
including lost upload responses. Conflicting attachment names/sizes or tag
commits fail explicitly. Historical releases and attachments are never deleted.
Gitee can expose a partial release while files upload; clients must not assume
`latest` has all files until synchronization finishes.
After all source attachments pass verification, `torto-update.json` is published
as the completion marker with the Windows installer's version, size and SHA-256.
Clients reject mirrors without this marker or whose marker does not match the
release tag and installer name. The manifest contains no credentials.

The Windows updater downloads GitHub installers through the Release Asset API
using the numeric asset ID from the existing update-check response and
`Accept: application/octet-stream`. No extra metadata lookup or client token is
needed. Failed or invalid downloads fall back to the ordinary GitHub release
URL, then to the same version and filename on Gitee. Every source must match
the selected installer's size and SHA-256. If only Gitee's release check
succeeds, its verified attachment remains the primary source, with the ordinary
GitHub release URL as fallback; no GitHub asset ID is guessed.

To retry or backfill, use Actions → Sync releases to Gitee → Run workflow and
enter `v0.7.0` or `latest`. This requires no installer rebuild.

Local read-only validation (Node.js 22+):

```sh
node --test scripts/sync-gitee-release.test.mjs
node scripts/sync-gitee-release.mjs --tag v0.7.0 --dry-run
```

The script does not modify the desktop updater; Gitee update-source fallback is a
separate change. Gitee upload permissions/quotas and public visibility still
apply. Logs omit authentication headers and API response bodies.
