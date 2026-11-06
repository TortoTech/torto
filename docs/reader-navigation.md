# Reader navigation

The Rust reader resolves TOC rows by their stable tree identity. Desktop and
Android use `go_to_toc` / `try_go_to_toc` instead of treating a row's URL as its
identity. Authored URLs in `Book` and `TocViewItem` remain unchanged. Ordinary
internal links continue to navigate their exact authored URL; their source-link
lookup is shared by both hosts.

## Target recovery

Recovery runs while preparing an affected section on a worker. Valid authored
fragments win. Only missing fragments or distinct sibling labels sharing one
fragment-less file URL are eligible for recovery. Parent/child aliases and fixed
layout publications are preserved.

Match a unique authored heading first, normalizing whitespace and case. A
separate heading ordinal can be combined with its immediately following title.
Do not match an ordinary paragraph, quotation, code sample or TOC label merely
because it contains the same words. Copyright entries without a heading may use
a unique explicit copyright anchor, then a unique paragraph beginning with ©
and containing a four-digit year among the first 64 blocks. Ambiguous candidates
retain the file-level fallback.

View wrappers provide `BookSource::navigation_section` to recover from authored
text before translation, AI layout and sentence splitting. A plain source returns
`None` and reuses its already parsed section. A recovered source anchor must
still exist in the displayed IR. Virtual fragment identifiers are added only to
prepared IR: original files, TOC labels/IDs, durable locator URLs and source
identities are not rewritten.

Candidate positions must be nondecreasing in TOC order. Prefix/suffix source
bounds include valid authored anchors and recovery proposals; inverted recovery
pairs are rejected on both sides. Equal positions remain valid for parent/child
aliases. Accepted targets define source-ordered reading-unit boundaries;
unresolved duplicate fallbacks do not steal the current-chapter highlight.
Forward/backward unit navigation uses physical source order and deduplicated
boundaries, never a chain of guessed TOC links.

## Caching and interaction

Index TOC rows per section once, without parsing the whole book at startup.
Prepare heading candidates only for eligible sections. Cache resolution and
source order inside the existing bounded, weakly retained `PreparedSection`
lifecycle. Source refresh invalidates that cache. Selection of the current TOC
entry uses source intervals first, with page positions as a fallback, so font,
viewport and translation changes do not require guessing against displayed
titles. Interactive destination parsing and pagination use the existing
coalesced prefetch worker. No AI request is involved.

## End-of-book page

A forward page turn at the actual book boundary, or a forward section turn from
its final reading unit, opens a synthetic completion page. Entering it does not
mark the book finished. An explicit “Mark finished” action records the status;
already finished books show that state and the library’s cumulative finished
book count. Both hosts show reading time, days, notes and recent reading history,
with a separate “Exit reading” action. Sharing and recommendations are omitted.

Backward navigation dismisses the synthetic page without issuing another native
turn, preserving the exact last source position and focus. Desktop uses the
configured previous-page/paragraph and previous-section shortcuts, plus Page Up
and Escape. These keys are handled before side panels, cancel spatial button
focus traversal, and explicitly wake the restored body frame. Wheel-back works
only when the statistics are already scrolled to the top.

Desktop statistics load on a single background job per open page. Results are
published before waking the UI; closing or replacing the page discards its
receiver. Loading, loaded and failed states do not schedule polling repaints.
Android serializes native boundary turns, marking and statistics through the
existing reader operation lock and rejects results from older book generations.

## Current verification

- Local read-only corpus: 78 reflowable publications (73 EPUB, three AZW3, one
  MOBI and one CHM), 5,055 documents and 9,350 TOC entries. All 5,351 valid
  authored fragments retain their targets; no new parse failure was found.
- 234 sampled navigation pairs compare row navigation against ordinary authored
  URL navigation. Original TOC metadata is checked for immutability.
- *The Algorithm Design Manual* recovers Copyright to `id_1/n18`, Preface to
  `id_1/n24` and Contents to `id_1/n61`. Book/unified presentation and forward/
  backward reading-unit transitions are covered. The title-page fallback no
  longer incorrectly highlights Contents.
- Two other EPUBs have ambiguous sibling file targets without unique headings
  or copyright markers. They retain file-level fallbacks rather than guessed
  content targets. Thirty fixed-layout PDFs do not enable heading recovery.
- Full workspace: 1,903 passed, 0 failed, 132 intentionally ignored. The real
  translated reading pipeline check passes without model calls. Four local PDFs
  pass eight native GPU zoom/selection/scroll tests; production desktop and
  Android builds pass. Five API 36 ARM64 emulator tests include the new JNI
  duplicate-URL TOC navigation and bidirectional unit-order case in both modes.
- Synthetic regressions cover heading priority over prose, duplicate headings,
  valid fragment preservation, parent/child aliases, translation and resize,
  authored internal links, inverted candidates and bidirectional termination.

The ignored reader test
`local_library_preserves_valid_authored_toc_and_bounds_heading_recovery` accepts
`TORTO_TOC_AUDIT_MANIFEST`, a JSON array of `{ "title": "…", "path": "…" }`.
Set `TORTO_TOC_AUDIT_OUTPUT` to save its report. The ignored app-core test
`local_translated_pipeline_keeps_authored_toc_recovery` accepts
`TORTO_TOC_PIPELINE_BOOK` pointing to the reported EPUB. These checks do not use
credentials, call models or write personal progress/publication files.
