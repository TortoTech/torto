# Repository Instructions

## GitHub Release Notes

- Write release notes in English first, followed by the Simplified Chinese translation inside an HTML `<details>` block.
- The summary tag must be exactly `<summary>中文更新说明</summary>`. Do not rename it or add attributes; the desktop updater uses this exact marker to select notes for the current interface language.
- Write for ordinary users. Lead with what changed in their experience and why it is useful, rather than how it was implemented.
- Prefer plain language and avoid library names, code symbols, architecture details, and other technical terminology unless users need them to understand compatibility or take action.
- Keep the notes concise. Use one short sentence per entry, combine closely related changes, and omit internal maintenance that has no meaningful user-facing effect.
- Classify changes under these headings and keep this order in both language sections:
  1. `## Feature` for new user-facing capabilities.
  2. `## Improvement` for enhancements to existing behavior, usability, performance, or quality.
  3. `## Fix` for corrected defects or regressions.
- Assign each change to one primary category and mention it only once per language. Do not repeat the same work under multiple headings with different wording.
- When a new feature includes supporting refinements, compatibility work, or corrections required to deliver that feature, describe them together in the `Feature` entry. Do not duplicate them as separate `Improvement` or `Fix` entries.
- Distinguish `Improvement` from `Fix` by intent: use `Fix` when previously intended behavior was incorrect, and `Improvement` when existing correct behavior was intentionally enhanced.
- Split work across categories only when the changes are independently meaningful to users and can each stand alone. Prefer one concise entry classified by its primary user-facing outcome when the distinction is uncertain.
- Keep the English and Chinese sections as one-to-one translations with the same categories and item order; neither language section should introduce additional or duplicated entries.
- Use the same English category headings in the Chinese section. Omit a category when it has no entries; do not invent filler items merely to include all three headings.
- Use this structure:

  ```markdown
  ## Feature

  - English description of a new capability.

  ## Improvement

  - English description of an enhancement.

  ## Fix

  - English description of a correction.

  <details>
  <summary>中文更新说明</summary>

  ## Feature

  - 新功能的中文说明。

  ## Improvement

  - 现有功能改进的中文说明。

  ## Fix

  - 问题修复的中文说明。

  </details>
  ```

- Keep all English-only content before `<details>`. Put all Chinese-only content inside the matching `<details>` block.
- If a Full Changelog link should appear in both languages, include it in both sections rather than placing it after `</details>`.
