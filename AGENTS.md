# Repository Instructions

## Desktop Performance

Apply these principles when changing the shelf, statistics, reader, settings, or background tasks:

- **Idle rendering:** Stop repainting once layout and animations settle. Avoid unconditional repaint requests, redraw feedback loops, and UI polling for unfinished tasks. Use deadlines for finite animations and timers; pause offscreen animations and reduce work when unfocused or minimized.
- **UI responsiveness:** Run database access, filesystem work, image decoding, and other blocking work in background tasks. The UI must never wait for workers or locks. Coalesce duplicate requests and publish results before sending a reliable completion wakeup.
- **Scheduling and stale results:** Preserve repaint request coalescing and deadline scheduling. Keep worker notifications independent of frame filtering. Reject stale results after book, account, configuration, or cache changes; cancellation alone may not stop blocking work.
- **Bounded caches:** Reuse shared caches and preserve memory budgets, eviction, disk reuse, and failure caching. Avoid retaining whole-library assets or duplicating image and formula pixels. Release obsolete references when books close or resources change.
- **Visible work only:** Keep list virtualization and skip offscreen loading and rendering. Generate thumbnails for display size and DPI, reuse suitable textures, and keep placeholders stable. Filtering and sorting should copy only necessary metadata.
- **Quiet background work:** Keep periodic checks in background timers and notify the UI only when needed. Preserve debouncing, bounded delays, retries, and required full synchronization.
- **Memory diagnosis:** Distinguish live allocations, CPU caches, GPU resources, and driver-retained capacity. A high working set or delayed memory reduction alone does not prove a leak; check resource ownership and cache retention.
- **Verification:** Build the production code path before runtime checks and run relevant existing regression tests. Check settled idle behavior, loading completion, popup closure, and focus/minimize transitions on affected screens. Compare equivalent books, builds, window sizes, DPI, and background activity; record GPU/driver details. Keep diagnostics lightweight and report measured improvements only when actually measured.

## GitHub Release Notes

- The GitHub Release title must exactly match its tag, including the lowercase `v` prefix (for example, `v0.5.0`). Do not add the product name, dates, subtitles, or descriptive suffixes. This is the release title, not a heading in the release notes body.
- When publishing with the GitHub CLI, explicitly use the tag as the title (for example, `gh release create v0.5.0 --title "v0.5.0" --notes-file <path>`), and verify the published title matches the tag.
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

### Contributor Attribution

- Before publishing, inspect the commits and associated pull requests between the previous release tag and the new tag. Include only contributions shipped in that release; check GitHub's commit-to-PR associations so squash/rebase merges are also covered.
- Credit externally contributed changes directly in their `Feature`, `Improvement`, or `Fix` entry, with links to the author's GitHub profile and the contributing PR. The maintainer's account is `L-Chris`; omit attribution for the maintainer's own changes.
- Use the PR author and verified co-authors as the contribution source. Do not substitute the person who merged the PR or the merge commit's committer. Confirm the contribution against the included code before assigning credit.
- Preserve identical authors and PR links in the English and Chinese versions of an entry. Use absolute GitHub URLs so the links also work when release notes are mirrored to Gitee.
- If an entry combines work from multiple authors, identify their respective contributions or split independently meaningful changes into separate entries. Do not attribute the maintainer's additional work to an external contributor merely because the entry also includes their PR.
- For a verified external contribution without a PR, link its commit. Do not guess authorship from a display name or an unverified email address.
- Follow the entry-level attribution used by [egui](https://github.com/emilk/egui/releases/tag/0.36.2) and [bat](https://github.com/sharkdp/bat/releases/tag/v0.26.1). Keep the existing release categories and concise user-facing descriptions.
- Example English entry: `Keep the table of contents highlight on the section currently being read. — by [@Catapult291](https://github.com/Catapult291), [#3](https://github.com/TortoTech/torto/pull/3)`.
- 对应中文条目：`目录高亮正确跟随当前阅读的章节。——感谢 [@Catapult291](https://github.com/Catapult291)，[PR #3](https://github.com/TortoTech/torto/pull/3)`。
