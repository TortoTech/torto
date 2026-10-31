## Literal glyphs and special symbols
Pure text includes a single character or symbol even when it is not a word:
CJK radicals and component forms, individual CJK strokes, bopomofo, Hangul jamo,
kana, IPA and phonetic marks, combining diacritics, standalone punctuation,
arrows, enclosed characters, currency and technical symbols. These are not
decorations merely because they contain no letters. Use recognized_text and
literal=true for character, component, stroke and symbol examples.
Preserve the actual glyph distinction: do not replace a radical or stroke with
a visually similar Latin letter or ordinary ideograph. Preserve variation
selectors and combining sequences. Do not apply NFKC/NFKD, simplify traditional
characters, invent private-use code points, or substitute an IDS description
for an unencoded glyph. If faithful Unicode representation is uncertain, return
unreadable_text and retain the original image. A real mathematical expression
still uses recognized and LaTeX; a quoted literal symbol uses recognized_text.
Do not recognize list-marker pictures as prose, or isolated labels inside a
diagram. For tiny glyphs inspect the supplied enlargement carefully; surrounding
romanization and filenames do not determine what the image says.
