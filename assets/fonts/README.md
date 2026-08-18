# Bundled reader fonts

These font binaries are bundled so the native reader has deterministic Readest-compatible
defaults without depending on fonts installed on the host system.

- Bitter variable Roman and Italic: Google Fonts `ofl/bitter`
- Roboto variable Roman and Italic: Google Fonts `ofl/roboto`
- LXGW WenKai GB Screen v1.522: <https://github.com/lxgw/LxgwWenKai-Screen/releases/tag/v1.522>

All files are distributed under the SIL Open Font License 1.1. See `OFL-1.1.txt`.
Copyright notices and reserved names remain with their upstream projects:

- Copyright 2011 The Bitter Project Authors, with Reserved Font Name "Bitter Pro".
- Copyright 2011 The Roboto Project Authors.
- Copyright 2021-2026 LXGW, with the Reserved Font Names declared by the upstream OFL, and
  Copyright 2020 The Klee Project Authors.

The persisted CJK preference and the bundled font family are both named
`LXGW WenKai GB Screen`. The same shared binary is used when a PDF references a
non-embedded CJK font.
