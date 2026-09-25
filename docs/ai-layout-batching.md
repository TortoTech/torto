# AI layout scheduling

The screen-visible range (or the existing translation request range when enabled)
activates directory subsections. Each activated subsection has a fixed plan based
on original source blocks, independent of scroll position and generated layout.

Each batch contains at most 5,000 source text characters, with no block-count limit.
Complete blocks and known semantic groups are indivisible: an oversized group is
sent alone. Media/caption runs and quote/attribution groups remain together. The
character budget excludes prompt/schema overhead and read-only boundary context.
Split batches receive at most one neighboring semantic group on each side, within
the same subsection. An unsplit subsection has no extra outside context.

Visible batches run first; remaining batches in the activated subsections follow
in source order. There is one active layout job. Scrolling inside its subsection
does not cancel it. New work in a different subsection can preempt an offscreen
job; without replacement work, the existing job is allowed to finish. Leaving a
subsection removes its unstarted batches from demand. Completed results remain
cached and staged until relevant for publication. Configuration/content changes
still invalidate incompatible jobs and caches.

Text recognition processes each scheduled batch in one request, subject to the
existing invalid-output retry. Image formulas retain their separate bounded image
batches. Translation keeps its existing approximately 2,000-character batches and
per-paragraph semantic readiness barrier.
