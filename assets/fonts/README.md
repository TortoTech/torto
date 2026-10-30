# Downloadable reader fonts

Production applications download these fonts in the background and retain them in the
application data directory across upgrades. They are not embedded in the executable or
copied into installers. The checked-in binaries remain offline regression fixtures and
resources for older clients.

`manifest.json` pins the size and SHA-256 of each font independently of the app version.
The primary source is the `reader-fonts-v1` release in
<https://github.com/TortoTech/lab/releases/tag/reader-fonts-v1>. Each font pins a
numeric GitHub asset ID; the client downloads it through the Release Asset API
with `Accept: application/octet-stream`, following binary redirects. It does not
query the latest release or require a GitHub token.

Gitee mirrors are uploaded manually to the matching `TortoTech/lab` release;
there is no automatic synchronization workflow. An optional per-font
`mirror_url` records the actual attachment download URL only after its anonymous
download, size, and SHA-256 have been verified. Missing or unverified mirrors are
omitted rather than pointing users at a login page. Source failures and invalid
downloads fall through to the next configured source.

Moving the download source does not change resource version `1`, IDs, font
bytes, or hashes, so existing cached fonts remain valid without downloading
again. Changing font content requires a new resource version and new hashes;
publish and verify the new attachments before updating the client manifest.
Do not replace published assets or remove the old Torto resources used by
already released clients. See `lab/docs/reader-fonts.md` for the manual release
and mirror checklist.

- Literata variable Roman and Italic (`opsz`, `wght`): Google Fonts `ofl/literata`
- LXGW WenKai GB Screen v1.522: <https://github.com/lxgw/LxgwWenKai-Screen/releases/tag/v1.522>

All files are distributed under the SIL Open Font License 1.1. See `OFL-1.1.txt`.
Copyright notices and reserved names remain with their upstream projects:

- Copyright 2017 The Literata Project Authors (<https://github.com/googlefonts/literata>).
- Copyright 2021-2026 LXGW, with the Reserved Font Names declared by the upstream OFL, and
  Copyright 2020 The Klee Project Authors.

The persisted CJK preference and the downloadable font family are both named
`LXGW WenKai GB Screen`. The same shared binary is used when a PDF references a
non-embedded CJK font.
