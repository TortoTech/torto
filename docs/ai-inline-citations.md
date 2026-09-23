# Inline bibliographic citations

AI layout includes a separate, schema-constrained classification pass for inline
literature references. Its Markdown prompt lives in
`apps/desktop/src/plugins/semantic_layout/citations/prompt.md`. It reuses the
existing AI layout model selection and chapter cache, with both prompt and schema
included in the request fingerprint. No additional setting is required.

The client finds bounded parenthesis/bracket candidates and sends contextual
excerpts in batches. The model returns candidate IDs only. Author–date,
author–page, Chinese and numeric citation formats are supported; ambiguous
numeric brackets, explanatory asides and narrative year-only parentheses remain
unchanged. Already identified footnotes and formula spans are protected.

Each accepted group gets one ordinal, starting at 1 in its original paragraph.
Multiple works in one parenthetical group stay together. Ordinals are stored on
the existing text runs, survive sentence structuring and do not restart at page
boundaries. Raw text and styles remain in the reading IR. Layout alone replaces
the reference with a compact `[n]` marker, retaining an expansion map for copy,
selection and durable source offsets. Disabling the overlay restores the source.

The existing footnote popup lists footnotes first, followed by citations in source
order. Citation rows have a matching numbered marker and a hanging text column.
Clicking an icon opens the popup and scrolls its row into view. Classic mode also
uses the existing footnote hover behavior. Bilingual companion icons retain the
original paragraph owner; translated text is annotated only when original citation
strings can be located without ambiguity. Unmatched translations stay intact.

## Verification

Deterministic coverage includes split styles, protected footnotes, cache validation,
overlay removal, mixed footnote/citation ordering, page fragments, long ordinals,
source offsets, copied text and bilingual icon hit regions.

The ignored `live_computational_models_inline_citations` test uses the configured
`gemini/lite` provider and `TORTO_CITATION_BOOK` pointing to a local copy of
*Computational Models of Reading*. It tests the photographed paragraph in chapter
2, including its four reference groups and ordinary explanatory asides. No book
content is bundled with the tests. Live tests make provider requests; passing this
sample is not a general citation-accuracy estimate.
