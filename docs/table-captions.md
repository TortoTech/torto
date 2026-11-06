# Table captions

Table captions are recognized during HTML parsing, independently of AI layout.
Both native HTML grids and images carrying tables can have preceding or following
captions. Image carriers retain the existing image preview and figure layout.

## Unified column layout and previews

Rectangular, reflowable text grids use a local bounded A* implementation in the
shared Rust layout engine, without an external solver dependency. Candidate
heights come from the same pre-shaped glyphs and Parley wrapper as final cells;
search does not repeat font shaping. Bounds remain valid without assuming rich
text heights are monotone. Optimality applies only to the measured candidates.
Tables that already fit their natural widths skip search. Merged cells,
width-dependent media/formulas and oversized grids retain the existing adaptive
layout. Source identity, captions, notes and continuation geometry are unchanged.

Limits are 256 cells, 10 columns, 3,072 candidate cell reflows, 4,096 generated
search states and 512 frontier states, with an 8 ms cooperative profile/search
budget. Budget exhaustion retains a feasible incumbent; profile failures are
cached too. Each engine retains at most 16 width plans keyed by actual displayed
text, styles, width and source identities. Translated text is remeasured, including
its writing-system font choices. The ordinary section/page cache still owns
prepared layouts.

Desktop previews use available window width with a 1,440 logical-pixel cap and
natural-width limits for short grids. Translation, target-language, semantic,
font or viewport changes coalesce a new worker request, reject older results and
retain the old surface while preparing its replacement. Explicit host content
versions cover mutable wrappers that do not supply `content_revision`.

The offline `rebook-app-core` example `table-layout-bench` accepts local book paths
and reports cold/warm whole-section times and solver counters. A controlled dev
(opt-level 1), Apple M6 comparison alternated old/new/new/old built binaries across
6 books, 10 table-rich/large sections and widths 800/1200 (font size 20, default
double-page configuration). Each pass used one cold plus three repeated layouts.
The sum of the 20 warm case medians was 1,881.8 ms before and 1,877.2 ms after;
this is approximately unchanged, not a demonstrated speedup. The 22-table,
1,396-cell *Handbook of Human Factors and Ergonomics* section measured
112.1→113.3 ms at width 800; the 11-table *On Food and Cooking* section remained
about 265 ms. Small *PDF Explained* cases added about 0.1–0.2 ms warm and up to
about 0.9 ms cold. No compilation overlapped these measurements. These are CPU
layout costs, not first-open I/O, GPU times or Windows/Android frame measurements.

## Recognition

- Native `caption` elements belong to their table. Cascaded/inherited
  `caption-side: bottom` places them after the grid.
- Numbered `Table` / `表` / `表格` labels and explicit table-caption classes can
  associate adjacent paragraphs with grids or images.
- A table container establishes ownership across transparent wrappers. Generic
  title/caption classes and explicit `NOTE:` / `Source:` paragraphs are accepted
  within that scope, preserving their order.
- Empty paragraphs and anchor-only elements do not prevent association. Their
  anchors are retained. Nested document wrappers from imported HTML are treated
  as block boundaries, so embedded grids are not flattened into prose.
- Prose references, ambiguous labels between unscoped grids, and titles inside
  table cells are not moved into captions. No OCR is required.

## Reading IR and presentation

`TableBlock.before` and `after` hold text in authored order. `Caption` denotes a
title/caption; `Paragraph` denotes an explicit table note. `text_blocks()` and
`text_blocks_mut()` traverse captions, cells, and notes in reading order.
Both new fields deserialize to empty lists for older serialized tables.

Unified layout reuses figure-caption typography and spacing: a one-line caption
is centered, a multiline caption is start-aligned, and notes are start-aligned.
Authored typography remains available in book layout. Pagination keeps a leading
caption with the first safe row group and reserves room for trailing text beside
the last rows when the group fits on a page. Captions are not repeated per page.

Focus selection, footnotes, copy, search, translation, AI context, and inline
content traversal include the attached text. Original text and source ranges are
retained; translated text stays with its table in replacement and bilingual modes.

## Validation

Parser fixtures cover both positions, image carriers, native captions, cascaded
CSS, wrapped consecutive tables, notes and references, Chinese labels, ambiguous
neighbors, and authored cell headings. Layout tests cover short/long tables at
three viewport heights; desktop tests cover translation and focus/copy/footnotes.

The ignored formats test `local_table_caption_survey` accepts `TORTO_TABLE_BOOK`.
Set `TORTO_TABLE_EXPECT_NO_CAPTIONS=1` for a negative fixture. Local book content
is not checked into the repository.

Local EPUB checks:

| Book | Parsed grids | Attached text before / after |
| --- | ---: | ---: |
| Computational Models of Reading | 29 | 29 / 0 |
| How We Read Now | 32 | 0 / 4 (including one note) |
| PDF Explained | 33 | 26 / 0 |
| 学习观 从感受到真正学会.md | 10 | 10 / 0 |
| The Hand | 2 | 0 / 0 |

Counts refer to parsed grid blocks, not raw HTML tag counts; some books use tables
for page layout rather than labeled data. These checks establish fixture coverage,
not a general recognition-accuracy estimate.
