# Formula images in AI layout

The existing AI layout provider/model is used for image transcription. This pass
runs before caption detection on the current chapter. Existing MathRun content,
fixed-page text layers, captioned figures and separators are excluded. Unassociated
small images, inline images and images with equation hints are candidates; filenames
and dimensions only nominate candidates, never prove that an image is a formula.

Requests use a Markdown prompt, image input and strict JSON Schema. The first
batch performs transcription and visual self-check together, returning
`recognized`, `not_formula` or `unreadable`, LaTeX without delimiters, and an
optional equation number printed inside the image. Recognized results pass the
native math parser/rendering, source/nesting limits and bounded geometry checks.
Valid results are accepted without another model call. These checks cannot prove
that every mathematical symbol matches the original; uncertain model results
continue to use the original image.

Only recognized proposals that fail local validation enter a second vision batch.
That request includes the original image, proposal and specific validation error;
a rendered comparison is attached only when it can be generated within the same
safety limits. Corrections must pass local validation before use. Failed review
keeps the original for that item without discarding valid siblings.

Both stages batch at most eight images and six MiB of payload. Responses are
matched by `image_id`, not array position. Only missing, duplicate-ID or malformed
items are retried once per stage; locally invalid transcriptions are routed to
review instead of resending successful siblings. There is no per-image fan-out.

Formula metadata is attached to the original image, preserving its resource,
source anchor and author styles. Unified typesetting uses the native LaTeX/SVG
renderer for display and inline formulas. Book typesetting keeps the original
image. Display numbers from HTML take priority without duplication; conflicting
numbers retain the original representation. Inline formulas use mathematical
ascent/descent metrics. Footnote popup text can also render recognized formulas.

Rendered image hits retain original pixels. Clicking a converted formula opens
the original-image preview, with a Copy LaTeX action. Failed renderings keep the
original raster; unreadable formulas are protected from subsequent caption
classification even though no transcription is displayed. Disabling AI layout
removes the overlay without changing the EPUB.

Image cache keys include image-byte hash, provider endpoint/model, prompt and
schema. Positive results and definitive negative results are reused; transient
verification failures are not cached. Existing chapter request fingerprints also
include this contract. Formula errors do not fail other AI layout recognizers;
unsupported image-input errors stop the formula pass for that chapter.
Batch transport uses a separate schema/prompt while preserving the existing
per-image semantic cache contract and completed chapter caches. Equal image bytes
under different resource names are deduplicated within the pending batch or reused
from results already obtained in the chapter.

## Verification

Unified display formula images reserve horizontal padding of `max(0.4em, 6px)`
and vertical padding of `max(0.25em, 4px)`, based on the display-math font size.
Contents are fitted before the transparent outer box is composed, so width/height
fitting does not consume the border's safety gap. Numbered rows retain a centered
formula and a right-aligned number; very narrow rows place the number below.
The padded box owns layout, selection and hit-testing geometry. Inline formulas
keep their compact metrics. SVG rasterization applies the horizontal origin offset
once, avoiding asymmetric safety margins.

Copying a selected recognized formula or its preview copies raw LaTeX. Known
formula text remains available when book mode displays the original image.
Unrecognized images still use image copying. Clipboard pixels are prepared only
after a copy event, not during idle/scroll frames. Original preview pixels and
LaTeX are preserved independently of presentation padding.

Deterministic tests cover single-request success, conditional review, failure isolation, parser rejection, original resources/anchors, overlay
removal, unreadable-image protection, book/unified modes, inline geometry, external
numbers, rendering fallback, original-image preview pixels and math in footnotes.

With `TORTO_FORMULA_BOOK` set to a local copy of *Computational Models of Reading*:

- `local_formula_book_structure` inspects parsed chapter/appendix image placement.
- `local_computational_formula_layout` verifies the real A.2 display equation and
  the nearby inline fraction, including original image preview and book mode.
- `live_computational_formula_images` uses configured `gemini/lite` on five formulas
  and one diagram from chapter 2/appendix A, sent together. With no local validation
  failures the batch now needs one request; only anomalous items trigger review.
  It checks
  transcription, native rendering, equation numbers and negative classification.

These tests are ignored by default; no book contents or credentials are bundled.
