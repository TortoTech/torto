# Text formulas in AI layout

Text formulas share the existing text-semantics request with headings, quotes,
captions and citations. No formula-candidate regex and no separate model call are
used. Target blocks expose full `math_texts` containers with paragraph IDs and
canonical bold/italic/superscript/subscript markup. Links, citations, existing math,
images and breaks are protected boundaries. Context-only blocks are not targets.
These containers also supply target text for heading/quote/caption decisions:
the duplicate flattened paragraph is omitted, and quote body entries reference
the canonical paragraph ID. Style tags do not delimit formulas; continuous
expressions and chained relations should be returned as one complete span.

The strict response schema adds `formulas` entries with block and paragraph IDs,
an exact formatted source substring, LaTeX, and optional disambiguating adjacent
text (empty strings when unnecessary). Matching uses a local byte-to-source-character
map. Unknown IDs, ambiguous occurrences, overlapping spans and unsupported LaTeX
are rejected independently; valid siblings remain available through partial results.
An exact source substring with a unique occurrence does not require matching
redundant context. Multiple occurrences still require exact adjacent context to
select one position. Protected-content and script-boundary checks always apply.
Empty selections and ranges detaching or truncating a superscript/subscript are
also rejected locally. This checks model output, not a local candidate extractor.
Accepted citation spans take precedence over overlapping proposed math.

Generated MathRun values retain original styled text. Unified layout renders math
and preserves the original character footprint for source offsets and copying;
book layout and disabling AI layout restore the authored text runs. A standalone
expression uses centered display math and padding. Selection of a rendered formula
can copy its LaTeX through the existing formula interaction.

Before translation, the prepared original block includes confirmed math. A guarded
in-memory snapshot restores the corresponding MathRun values from existing math
placeholders when the response is applied. Raw block equality and placeholder
validation prevent reuse against edited source or incompatible old translations.
Untranslated content is not published early. Cache fingerprints include the new
schema and prompt; no ebook files are rewritten.

Tests include synthetic ambiguity/protection cases, translation and toggle behavior,
renderer copy/source mapping, and the photographed chapter paragraph from a local
copy of Why You Hear What You Hear (`TORTO_TEXT_FORMULA_BOOK`). Local-book tests are
ignored by default and do not call a model.
