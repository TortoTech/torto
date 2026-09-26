# Visible-content scheduling and coordinated publication

Translation retains its original page-level lookahead: it processes the complete
layout pages intersecting the viewport, including later blocks on those pages.
When translation and AI layout are both enabled, this envelope activates AI
subsections; standalone AI uses the actual screen viewport, including visible
neighboring blocks in focus mode. Hidden linked notes are added only for
translation. Switching modes invalidates the prepared range policy.

AI layout uses fixed directory-subsection batches with a 5,000-character target
budget and no block-count limit. Complete semantic groups are not split; an
oversized group forms its own batch. Visible batches run first, followed by the
remaining batches in the activated subsection. Split batches carry at most one
read-only neighboring semantic group on each side. See [AI layout batching](ai-layout-batching.md).

A 200 ms settle interval prevents request churn while navigating. One AI task is
active at a time. Scrolling within its subsection preserves it; new work in another
subsection can preempt an offscreen task. Leaving a subsection removes its unstarted
batches. Without competing work, an in-flight request may finish and be cached.
Closing the book, disabling AI, or changing its configuration cancels incompatible
tasks. Cancellation does not produce a failure notice; the server may still finish
work already received. Diagnostic logs link scheduling, demand changes, cancellation
reasons and completion by task ID.

Translation retains one active body request of approximately 2,000 characters,
without paragraph splitting. TOC translation remains separately cancellable.
An offscreen body translation is retained until another untranslated batch is
ready to run, including completion of its AI layout prerequisites. Returning to
any block in the active batch reuses that request. Configuration changes and
disabling translation still cancel incompatible work. Translation logs record
task IDs, block ranges, scheduling, start, cancellation reason, completion and
accepted result batches.
Image formula requests use at most five original images and approximately 6 MiB
of encoded data per batch. Text recognition, image transcription and corrective
retries use the selected AI layout reasoning effort. The AI task deadline remains
180 seconds.

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
