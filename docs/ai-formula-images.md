# Formula images in AI layout

The existing AI layout provider/model is used for image transcription. This pass
runs before caption detection on the current chapter. Existing MathRun content,
fixed-page text layers, captioned figures and separators are excluded. Unassociated
small images, inline images and images with equation hints are candidates; filenames
and dimensions only nominate candidates, never prove that an image is a formula.

Requests use a Markdown prompt, image input and strict JSON Schema. A first pass
returns `recognized`, `not_formula` or `unreadable`, LaTeX without delimiters, and
an optional equation number printed inside the image. Recognized results must pass
the native math parser/layout and bounded geometry checks. A second vision pass
compares the original with the native rendering, checking symbols, scripts and
fraction/summation structure. Unsupported/uncertain results retain the original.
This improves detection of transcription mistakes but cannot guarantee correctness.

Both passes are batched: at most eight images per batch, with a six-MiB image
payload budget. Verification includes original/rendered pairs and is split again
when needed to stay within that budget. Responses are matched by `image_id`, not
array position. Only missing, duplicate-ID or invalid items are retried once as a
batch; valid siblings are retained. Failed verification keeps the original image.
There is no automatic fan-out into one request per image.

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

Deterministic tests cover parser rejection, original resources/anchors, overlay
removal, unreadable-image protection, book/unified modes, inline geometry, external
numbers, rendering fallback, original-image preview pixels and math in footnotes.

With `TORTO_FORMULA_BOOK` set to a local copy of *Computational Models of Reading*:

- `local_formula_book_structure` inspects parsed chapter/appendix image placement.
- `local_computational_formula_layout` verifies the real A.2 display equation and
  the nearby inline fraction, including original image preview and book mode.
- `live_computational_formula_images` uses configured `gemini/lite` on five formulas
  and one diagram from chapter 2/appendix A, sent together. The checked batch used
  two requests (six-image transcription, five-image verification), compared with
  eleven normal requests for the former per-image flow. It checks
  transcription, native rendering, equation numbers and negative classification.

These tests are ignored by default; no book contents or credentials are bundled.
