# Table captions

Table captions are recognized during HTML parsing, independently of AI layout.
Both native HTML grids and images carrying tables can have preceding or following
captions. Image carriers retain the existing image preview and figure layout.

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
