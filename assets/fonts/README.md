# Downloadable reader fonts

Production applications download these fonts in the background and retain them in the
application data directory across upgrades. They are not embedded in the executable or
copied into installers. The checked-in binaries remain offline regression fixtures and
immutable download resources.

`manifest.json` pins the size and SHA-256 of each font independently of the app version.
The primary and mirror revisions point to already published GitHub and Gitee objects;
both must contain the identical checked-in bytes. Gitee requires a login for the large
CJK file, so its public fallback is the pinned official LXGW release instead. Changing a font requires a new resource
version, new hashes, and publishing/verifying both source objects before updating this manifest.

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
