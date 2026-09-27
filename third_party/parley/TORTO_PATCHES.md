# Local Parley changes

Based on the crates.io source of Parley 0.11.1, retaining its Apache-2.0/MIT licenses.

`Layout::reserve_inline_box_paint_bounds` extends finished line metrics to contain
inline images painted with a vertical offset. It moves subsequent lines and updates
the layout height, so rendering, pagination, selection and hit testing share bounds.
Horizontal wrapping and the image's alignment relative to text stay unchanged.
Torto calls it after its final line breaking and alignment pass.
