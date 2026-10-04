# Local Parley changes

Based on the crates.io source of Parley 0.11.1, retaining its Apache-2.0/MIT licenses.

`Layout::reserve_inline_box_paint_bounds` extends finished line metrics to contain
inline images painted with a vertical offset. It moves subsequent lines and updates
the layout height, so rendering, pagination, selection and hit testing share bounds.
Horizontal wrapping and the image's alignment relative to text stay unchanged.
Torto calls it after its final line breaking and alignment pass.

`Layout::reserve_text_paint_bounds` does the same for ruby annotations anchored to
source text ranges. Offsets are relative to the body baseline. A text range stays
with its base at a wrap boundary, unlike a zero-width inline positioning marker.

`RangedBuilder::set_base_level` supplies an authored paragraph direction to bidi
analysis, including Latin URLs and neutral icon placeholders. The override resets
when beginning each builder and adds no characters or source-offset shifts.
`set_ltr_ranges` gives complete translated paragraphs inside bilingual cells/notes
independent LTR bidi levels. `Layout::align_with_left_ranges` aligns those paragraphs
left while preserving original paragraph alignment; ranges also participate in
undoing justification before another line-break/alignment pass.
