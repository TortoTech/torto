# Visible-content scheduling and coordinated publication

Translation retains its original page-level lookahead: it processes the complete
layout pages intersecting the viewport, including later blocks on those pages.
When translation and AI layout are both enabled, all AI recognition uses that same
request envelope, including image-only formula blocks and linked footnotes.
Standalone AI layout uses the actual screen viewport, also in focus mode: visible
neighboring blocks are included, not only the active unit. Hidden linked notes are
added only for translation. Switching modes invalidates the prepared range policy
even when the visible source ranges happen to be identical.

A 200 ms settle interval prevents request churn while navigating. No fixed number
of successor blocks/subsections is queued beyond the translation page envelope.
Full paragraphs, figures, quotes and tables remain
intact. Already completed results and unchanged in-flight batches are reused.
When an active batch no longer intersects current demand, its Tokio task is aborted
and its callback channel/task ID is invalidated. Closing the book, disabling a
feature, or changing its backend also cancels its owned task. Cancellation does not
produce a failure notice. A server may still finish work already received.

Translation has one active body request, with approximately 2,000 characters per
batch and no paragraph splitting. Its next batch is selected again from current
demand. TOC translation remains a separate cancellable task. AI layout has one
active batch of at most 16 adjacent requested blocks. Up to six neighboring blocks
on either side supply context, bounded by resolvable TOC anchors. Context-only
proposals are rejected; a validated body/source or image/caption relationship may
include a context block as a dependency. Formula and citation requests target only
the selected request envelope. The full AI batch has a 180-second deadline.

## Completed versus displayed

Successful translations are staged rather than immediately published. AI results
are partitioned into independent paragraph groups, joining paragraphs referenced
by the same semantic annotation. Overlapping pending groups share a barrier.
Negative recognition results and failures also complete the AI side of the barrier.
A group becomes displayable when its AI result and all required translation
segments are ready, failed, or disabled. A failure leaves that side's original
content intact and releases the other side's successful result.

Visible ready groups commit translation and semantic overlays under one shared
publication lock. Background parsing cannot observe the middle of that commit.
All groups ready in the same tick share one anchored reader refresh. Selection,
annotation editing and focus-scroll animation postpone publication. Offscreen
completed groups remain staged until revisited; completed network caches are kept.
Source fingerprints reject stale results after original text changes. Configuration
identity includes credentials as well as provider, model and target language.

Recognition continues to use original/rewrite text; semantic composition remains
after translation, preserving translation block/segment keys. Changing translation
mode does not require re-running semantic recognition. If translated citation text
cannot be located unambiguously, it remains intact rather than being replaced at a
guessed range.

## Validation

Layout publication also runs in the blocking pool. The old reader pages remain
interactive until a refreshed reader has compiled the whole current reading unit.
Adoption checks content version, style, viewport and reading unit, and restores the
latest source anchor from compiled pages only. Old reader disposal is off-thread;
prefetch worker destruction signals cancellation without joining an active parser
on the UI thread. Image focus keeps shared RGBA pixels and performs clipboard color
conversion only on copy. The image-atlas compatibility refresh remains enabled,
with sampled diagnostics instead of per-frame file writes.

Viewport inspection does not parse chapters. A blocking-pool preparation task
resolves linked notes, fingerprints and translation inputs, then publishes an
immutable snapshot to the scheduler. Polling filters the prepared index against
completed translations. Canonical reading IR is cached separately from rewrites,
so display refreshes reuse parsing while still applying current edits. Rewrite
revisions invalidate prepared work; PDF OCR mode switches also clear canonical IR.
The warm-scheduling regression checks parser call counts across viewport changes
and 100 readiness polls. `local_translation_planning_performance` accepts a local
EPUB through `TORTO_PERF_BOOK` and compares indexed versus reparsing selection.

Regression tests cover actual future cancellation and stale callback rejection,
viewport image inclusion, screen-visible focus-mode demand, translation page lookahead,
context-versus-target request payloads,
related-group dependencies, independent paragraph groups, offscreen retention,
failed/disabled-side release, overlapping barriers, and credential/target changes.
The existing translation/AI completion-order and source-range regression tests
remain applicable. Tests use local fixtures and a loopback model stub, without
sending book content to a remote model.


## Citation-aware translation dependency

When both features are enabled, each translation input waits for AI layout to
finish for its original block. Other completed blocks can proceed without waiting
for the section. Empty, failed, or timed-out recognition releases that dependency;
cancelled work does not. The existing request envelope, preemption, approximately
2,000-character translation batches, and atomic display commit remain unchanged.

Translation input is enriched from committed and staged inline-citation annotations
without publishing the staged layout or changing block/segment storage keys.
Each citation is enclosed once in `<citation id="N">...</citation>`, outside its
inline style runs. Responses must preserve every ID exactly once, with balanced,
nonempty groups. Invalid responses use the existing retry/failure path. The citation
contents may be translated, while the ID restores the interaction directly. Legacy
translations still use conservative text matching. Turning AI layout off expands
the translated citation contents without discarding them.


## Unified text semantics request

Each text window now submits quote, caption, heading and inline-citation candidates
in one request. Ordered blocks carry `citation_candidates`; protected composite
blocks expose their relevant text containers without becoming group targets.
`targets` lists block classification, existing quotes missing sources, and citation
candidate IDs separately. Context-only citation candidates are not submitted.

The strict response schema contains `groups` and `citations`. Candidate IDs map to
client-owned original source ranges; models never generate character offsets.
The two result sets are independently checked, and safe partial results survive a
failed retry. Window caches store both arrays. The request-contract fingerprint
invalidates incompatible old text recognition caches without a release version bump.
Formula-image batches remain separate. Headings are finalized in the unified request; there is no second heading review.
