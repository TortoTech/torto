# Bundled reader fonts

These font binaries are bundled so the native reader has deterministic reading defaults without
depending on fonts installed on the host system.

- Ysabeau Office variable Roman and Italic: Google Fonts `ofl/ysabeauoffice`
- LXGW WenKai GB Screen v1.522: <https://github.com/lxgw/LxgwWenKai-Screen/releases/tag/v1.522>

All files are distributed under the SIL Open Font License 1.1. See `OFL-1.1.txt`.
Copyright notices and reserved names remain with their upstream projects:

- Copyright 2023 The Ysabeau Project Authors (<https://github.com/CatharsisFonts/Ysabeau>).
- Copyright 2021-2026 LXGW, with the Reserved Font Names declared by the upstream OFL, and
  Copyright 2020 The Klee Project Authors.

The persisted CJK preference and the bundled font family are both named
`LXGW WenKai GB Screen`. The same shared binary is used when a PDF references a
non-embedded CJK font.
