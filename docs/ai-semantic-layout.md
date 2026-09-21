# AI layout

Settings → Typography → Layout contains a global, opt-in switch and a configured
provider/model selection. Quotes and captions are always both enabled. The help
icon next to the switch shows “补充未识别的引用、图注” on hover.
Credentials remain in the existing provider configuration. A missing or deleted
model pauses recognition; it is never silently replaced. There are no per-book
switches or manual chapter recognition commands.

## Pipeline

`parser → rewrite → translation → semantic layout → sentence structure → layout`

Recognition reads the rewritten, untranslated publication. Only source-backed
ordinary paragraphs and unassociated images are classification candidates. Existing semantic
blocks are represented as protected boundaries, without their text, except for
recognized quotes missing an attribution. Those are exposed only for credit
completion, without reclassifying or restyling their body. Headings
provide read-only context. Model output contains IDs and roles, never replacement
text or styles. Local validation rejects nonexistent IDs, overlaps, discontinuous
groups, nonadjacent captions and changes to existing semantics.
Images adjacent to an already recognized standalone caption are protected too,
including consecutive image runs. This prevents attaching ordinary surrounding
prose as a second caption. Local checks validate structure rather than require
typographic evidence: quotes may lack quotation marks, a preceding colon,
heading adjacency or a separate attribution paragraph. An inline credit stays
inside its original paragraph. An invalid optional attribution relationship is
discarded without erasing an independently valid quote body. Windows with images use
a separate caption pass, protecting accepted captions during quotation detection.

The system prompt is maintained as sectioned Markdown in
`apps/desktop/src/plugins/semantic_layout/prompt.md`: task, input and scope,
semantic judgment, structural requirements, output and examples. Retry feedback
also uses Markdown headings. Book input remains a separate JSON data message. Requests use
`response_format.type = json_schema` and `strict = true`, with a quote-only or
caption-only schema for each pass (required fields, nullable attribution/alignment and no
additional properties). This requires a compatible endpoint; API errors are
reported rather than silently falling back to unconstrained output. Local ID,
source-protection and relationship checks still run after parsing.

`quote_attribution` associates a source-backed credit with an existing quote that
has no attribution. It can consume the immediately following paragraph or move
the quote's separate final body paragraph into its attribution field. The latter
must leave at least one body paragraph. Both cases preserve source links and
bilingual companions. Already attributed quotes stay protected; inline credits
sharing a paragraph with prose are left in place. The model cannot generate new
credit text or infer an author absent from the supplied book content.

Validated IDs are resolved to source ranges. Composition happens after translation
so its block/segment indices remain stable. Inline content, links and source
ranges are retained. Bilingual companions remain with their original paragraphs.
The resulting `QuoteBlock` and `FigureBlock` use the existing layout and reader
interactions. Source documents are not modified.
For AI-detected quotes, `alignment` recommends `start`, `center`, `end`, `justify`
or `null`. It is persisted with the annotation and applied to all quote-body
paragraphs (including bilingual companions) as a separate semantic style hint.
Unified layout prioritizes that hint over the authored alignment, while retaining
its normal rule that `start` becomes `justify`. `null` retains normal reader behavior. Authored
styles remain intact for book mode; independently parsed quotes and separate
attribution paragraphs retain their existing rules.
The prompt favors `start` or `justify` for prose and dialogue, including chapter
epigraphs; centering requires intentional poetic lineation or a balanced dedication.
Unified quote bodies and attributions stay upright even when inline emphasis,
alternate-voice or citation semantics would otherwise restore italics.
Unified layout applies caption alignment to the whole figure caption: one line
is centered, while multiple lines share a leading edge even when the original
ebook split them into several paragraphs. Authored book-mode alignment is retained.

## Scheduling and persistence

Current chapter first, next chapter second; one request at a time. Each window
targets at most 48 blocks, with a nominal 16,000-character body budget and six
context blocks on either side. A single long paragraph remains intact. Requests
have a 90-second timeout; invalid structured output gets one corrective retry.
If a typed response still contains invalid groups, retain only groups passing all
local checks. Rejected groups remain unchanged. Partial windows/chapters are not
cached as complete, allowing another attempt on reopening without retrying every
redraw. Network errors and unreadable responses retain the original layout.

`data/logs/semantic-layout.log` records chapter start/completion/failure and window
validation retries, ranges and skipped group counts in both debug and release
builds. It does not record request bodies/model text and redacts provider secrets
from error details. The log rotates at 1 MiB, retaining one previous file.

Completed windows and chapters are stored under `cache/semantic-layout-v1`.
The unpublished cache format starts at version 1; this is not the application's
release version. Keys include publication identity, chapter content fingerprint,
endpoint/model, enabled roles and a content hash of the Markdown prompt, output
schemas and window parameters. Editing those request contracts invalidates old
results automatically, including cached omissions, without manual prompt-version
increments. Empty results are cached too. Full chapter
hits can be applied during parsing without an API request. Font, theme and size
changes do not invalidate recognition. Modified source content does.

Settings changes and navigation away from the current/next chapter cancel work.
Dropping the reader aborts its worker. Results are checked against current content
before installation, and held while selecting text, editing a note or animating
focus scrolling. Reflow preserves source anchors and focus screen position.
AI reflow restores that position by scrolling the viewport as a whole. It must
not reuse the sentence-structure adjustment that translates one paragraph's
paint commands independently: that can detach text from neighboring images or
overlap other text when preceding content changes height.
Original fixed-layout PDF pages are excluded.

## Verification

Run deterministic tests with:

```text
cargo test -p rebook-desktop semantic_layout
```

An explicitly ignored live test uses the already configured `gemini/lite` model
and a local copy of *Phantoms in the Brain* (displayed as `V.S. Ramachandran` in
the test library). It sends only the first 24 parsed blocks of seven selected
chapters. Expected quote bodies, credits and a multipart cartoon caption are
checked against manually inspected passages; neighboring narrative/dialogue
must remain ordinary paragraphs.
The release check requires the ten chapter epigraphs (including their credits)
and the cartoon caption, and rejects false positives. An additional unattributed
letter is tracked as a recall benchmark: `gemini/lite` missed it in the checked
run. The report preserves this miss rather than claiming perfect recognition.

Set `TORTO_SEMANTIC_BOOK` to that EPUB and optionally `TORTO_SEMANTIC_REPORT` to
a local JSON report path, then run:

```text
cargo test -p rebook-desktop live_ramachandran_gemini_lite -- --ignored --nocapture
```

The optional report contains book excerpts for local review. Neither the book nor
credentials are committed. The live test can incur provider charges. It does not
change saved settings. These samples are a regression check, not a general
accuracy estimate.

`live_hand_epigraph_with_inline_credit` exercises the entire fifth chapter of
*The Hand*, using the production windowing, configured model, cached chapter
loading and source overlay. It verifies that the Octavio Paz epigraph is a quote
both in the result and in the reader source; a short excerpt alone is insufficient
to reproduce the production request context.
