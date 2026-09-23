# AI layout

Settings → Typography → Layout contains a global, opt-in switch and a configured
provider/model selection. Quotes, inline citations, formula images, captions and numbered section headings are always enabled. The help
icon next to the switch shows “补充未识别的引用、文内文献引用、公式图片、图注和数字节标题” on hover.
Credentials remain in the existing provider configuration. A missing or deleted
model pauses recognition; it is never silently replaced. There are no per-book
switches or manual chapter recognition commands.

## Pipeline

See [Inline bibliographic citations](ai-inline-citations.md) for numbered inline
markers, source-preserving folding and the shared footnote popup.
See [Formula images](ai-formula-images.md) for visual transcription, verification,
original-image fallback and unified formula rendering.

`parser → rewrite → translation → semantic layout → sentence structure → layout`

Recognition reads the rewritten, untranslated publication. Only source-backed
ordinary paragraphs and unassociated images are classification candidates. Existing semantic
blocks are represented as protected boundaries, without their text, except for
recognized quotes missing an attribution. Those are exposed only for credit
completion, without reclassifying or restyling their body. Headings
provide read-only context. Model output contains IDs and roles, never replacement
text or styles, except for exact source-credit suffixes copied for localization.
Local validation rejects nonexistent IDs, overlaps, discontinuous
groups, nonadjacent captions and changes to existing semantics.
Images adjacent to an already recognized standalone caption are protected too,
including consecutive image runs. This prevents attaching ordinary surrounding
prose as a second caption. New quotes require an explicit source in one of three
positions: a following standalone credit (`quote`), a preceding named introduction
ending with a colon or explicit reporting cue (`quote_before`), or the exact credit suffix of the final body
paragraph (`quote_inline`). Unattributed prose, epigraphs and ordinary narrative
dialogue cannot become new AI quotes. Invalid credits reject the whole new group;
there is no body-only repair. Windows with images still use a separate caption pass.

The system prompt is maintained as sectioned Markdown in
`apps/desktop/src/plugins/semantic_layout/prompt.md`: task, input and scope,
semantic judgment, structural requirements, output and examples. Retry feedback
also uses Markdown headings. Book input remains a separate JSON data message. Requests use
`response_format.type = json_schema` and `strict = true`, with a quote-only, caption-only or
heading-only schema for each pass (required fields, non-null new-quote attribution, nullable alignment and no
additional properties). This requires a compatible endpoint; API errors are
reported rather than silently falling back to unconstrained output. Local ID,
source-protection and relationship checks still run after parsing.

`quote_attribution` associates a source-backed credit with an existing quote that
has no attribution. It can consume the immediately following paragraph or move
the quote's separate final body paragraph into its attribution field. The latter
must leave at least one body paragraph. Both cases preserve source links and
bilingual companions. Already attributed quotes stay protected. `quote_inline`
can also extract an explicitly written suffix from a recognized quote's final
body paragraph without recommending alignment or changing its recognized scope. The model cannot generate new
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

## Numbered section headings

An additional pass runs only for windows containing source-backed ordinary
paragraphs consisting of a positive Arabic numeral (up to four digits), optionally
followed by a period or closing parenthesis. This is candidate eligibility, not
an automatic heading decision. The model distinguishes subsection boundaries from
page numbers, footnotes, list/exercise numbers and other incidental numbering.
An increasing sequence alone is insufficient; uncertain candidates stay unchanged.
Roman numerals, spelled-out numbers and number-plus-title paragraphs are outside
this initial candidate filter.

The pass receives the normal body window and an ordered summary of up to 64
nearby numeric candidates across that source section, with bounded excerpts from
their immediate neighbors. Context-only IDs outside the target cannot be changed.
Captions are recognized first, headings second, quotes last; accepted groups are
protected in subsequent passes. Proposed headings receive a second, chapter-level
audit using neighboring prose and the broader numbering pattern; only IDs
accepted in both passes are installed. Review batches and excerpts are bounded.
The audit cannot add new candidates, and errors preserve the original content. `section_heading` returns only an existing block
ID. Validation enforces candidate eligibility, target ownership and non-overlap.

Annotations retain the canonical source range and compose after translation,
changing the original and its bilingual companion to `Heading(3)` without
rewriting text. Existing small-heading layout keeps the heading with following
body content. Focus navigation skips it in both directions while it remains
visible. No TOC entry or reading-unit boundary is created; disabling AI layout
restores the ordinary paragraph. Prompt/schema fingerprinting invalidates old
cached omissions when this recognition role is introduced.

The ignored `live_numbered_headings_and_book_page_numbers` test uses the configured
model to recognize three independent synthetic numbered subsections, then checks
the full third chapter of the local *Phantoms in the Brain* EPUB for false positives
on residual page numbers. Set `TORTO_SEMANTIC_BOOK` and run with `--ignored`.
Book passages remain test input, not production prompt examples. Synthetic recall
and this one-book rejection check do not establish cross-book accuracy.

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
the test library). It sends the first 24 parsed blocks of six selected chapters
and the entire eighth chapter. Expected quote bodies, credits and a multipart cartoon caption are
checked against manually inspected passages; neighboring narrative/dialogue
must remain ordinary paragraphs.
The release check requires the ten chapter epigraphs (including their credits)
and the cartoon caption, and rejects false positives. An unattributed letter
that used to be a recall target is now an explicit negative example.

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

## Explicit quote source presentation

Preceding introductions stay in place and their association is stored in the
source-backed annotation. Inline suffixes are split preserving runs, links and
character-based source ranges. A bilingual companion stays intact with its body;
a translated-only paragraph whose suffix no longer matches stays intact rather
than applying original offsets to translated text. Prompt/schema fingerprints
invalidate all earlier AI results automatically, including old body-only quotes.
The seven-chapter Ramachandran benchmark now treats the unattributed letter as
out of scope, and still requires the sourced chapter epigraphs and caption.
