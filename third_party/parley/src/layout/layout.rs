// Copyright 2025 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::layout::alignment::align;
use crate::layout::alignment::unjustify;
use crate::layout::data::LayoutData;
use crate::style::Brush;
use crate::InlineBox;
use core::cmp::Ordering;
use core::fmt;

use crate::layout::{
    alignment::Alignment, alignment::AlignmentOptions, line::Line, line_break::BreakLines,
    ContentWidths, Style,
};
use crate::IndentOptions;

/// Text layout.
///
/// The [`Debug`] implementation prints a compact summary by default.
/// The alternate form (`{:#?}`) formats the full underlying data.
///
/// [`Debug`]: core::fmt::Debug
#[derive(Clone)]
pub struct Layout<B: Brush> {
    pub(crate) data: LayoutData<B>,
}

impl<B: Brush> fmt::Debug for Layout<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            f.debug_struct("Layout").field("data", &self.data).finish()
        } else {
            f.debug_struct("Layout")
                .field("text_len", &self.data.text_len)
                .field("width", &self.data.width)
                .field("height", &self.data.height)
                .field("lines", &self.data.lines.len())
                .field("runs", &self.data.runs.len())
                .field("styles", &self.data.styles.len())
                .finish_non_exhaustive()
        }
    }
}

impl<B: Brush> Layout<B> {
    /// Creates an empty layout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the scale factor provided when creating the layout.
    pub fn scale(&self) -> f32 {
        self.data.scale
    }

    /// Returns the style collection for the layout.
    pub fn styles(&self) -> &[Style<B>] {
        &self.data.styles
    }

    /// The `max_advance` that was used to line break the `Layout`
    pub fn layout_max_advance(&self) -> f32 {
        self.data.layout_max_advance
    }

    /// Returns the computed width of the layout excluding the width of trailing whitespace.
    pub fn width(&self) -> f32 {
        self.data.width
    }

    /// Returns the computed width of the layout including the width of trailing whitespace.
    pub fn full_width(&self) -> f32 {
        self.data.full_width
    }

    /// Calculates the lower and upper bounds on the width of the layout. These
    /// are recalculated every time this method is called.
    ///
    /// This method currently may not return the correct results for
    /// mixed-direction text.
    pub fn calculate_content_widths(&self) -> ContentWidths {
        self.data.calculate_content_widths()
    }

    /// Returns the height of the layout.
    pub fn height(&self) -> f32 {
        self.data.height
    }

    /// Returns the number of lines in the layout.
    pub fn len(&self) -> usize {
        self.data.lines.len()
    }

    /// Returns `true` if the layout is empty.
    pub fn is_empty(&self) -> bool {
        self.data.lines.is_empty()
    }

    /// Returns the line at the specified index.
    ///
    /// Returns `None` if the index is out of bounds, i.e. if it's
    /// not less than [`self.len()`](Self::len).
    pub fn get(&self, index: usize) -> Option<Line<'_, B>> {
        Some(Line {
            index: index as u32,
            layout: self,
            data: self.data.lines.get(index)?,
        })
    }

    /// Returns `true` if the dominant direction of the layout is right-to-left.
    pub fn is_rtl(&self) -> bool {
        self.data.base_level & 1 != 0
    }

    pub fn inline_boxes(&self) -> &[InlineBox] {
        &self.data.inline_boxes
    }

    pub fn inline_boxes_mut(&mut self) -> &mut [InlineBox] {
        &mut self.data.inline_boxes
    }

    /// Expand finished line boxes to contain application-painted inline content.
    /// Each tuple is (box id, paint offset from box top, painted height).
    /// Call after line breaking and alignment; breaking again resets this adjustment.
    /// Text baselines relative to inline boxes and horizontal breaks are preserved.
    pub fn reserve_inline_box_paint_bounds(&mut self, bounds: &[(u64, f32, f32)]) {
        let extra: alloc::vec::Vec<_> = self
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let m = line.metrics();
                let top = m.block_min_coord;
                let bottom = self.get(index + 1).map_or(
                    m.block_max_coord.max(top + m.line_height),
                    |next| {
                        m.block_max_coord
                            .max(top + m.line_height)
                            .min(next.metrics().block_min_coord)
                    },
                );
                let mut above = 0.0_f32;
                let mut below = 0.0_f32;
                for item in line.items() {
                    if let crate::layout::PositionedLayoutItem::InlineBox(item) = item {
                        if let Some(&(_, offset, height)) = bounds.iter().find(|b| b.0 == item.id) {
                            if offset.is_finite() && height.is_finite() && height >= 0.0 {
                                above = above.max(top - item.y - offset);
                                below = below.max(item.y + offset + height - bottom);
                            }
                        }
                    }
                }
                (above, below)
            })
            .collect();
        let mut shift = 0.0;
        for (line, (above, below)) in self.data.lines.iter_mut().zip(extra) {
            let m = &mut line.metrics;
            m.block_min_coord += shift;
            m.block_max_coord += shift + above + below;
            m.baseline += shift + above;
            m.ascent += above;
            m.descent += below;
            m.line_height += above + below;
            shift += above + below;
        }
        self.data.height += shift;
    }

    /// Returns an iterator over the lines in the layout.
    pub fn lines(
        &self,
    ) -> impl ExactSizeIterator<Item = Line<'_, B>> + DoubleEndedIterator + '_ + Clone {
        self.data
            .lines
            .iter()
            .enumerate()
            .map(move |(index, data)| Line {
                index: index as u32,
                layout: self,
                data,
            })
    }

    /// Sets the text-indent for the layout.
    ///
    /// The indent is applied as a margin on the start edge of indented lines, reducing the
    /// available width for line breaking and offsetting content during alignment. Negative
    /// values cause the line to protrude beyond the start edge.
    ///
    /// This must be called before [`Layout::break_all_lines`] or [`Layout::break_lines`],
    /// and before [`Layout::align`].
    pub fn set_text_indent(&mut self, amount: f32, options: IndentOptions) {
        self.data.indent_amount = amount;
        self.data.indent_options = options;
    }

    /// Returns line breaker to compute lines for the layout.
    pub fn break_lines(&mut self) -> BreakLines<'_, B> {
        unjustify(&mut self.data);
        BreakLines::new(self)
    }

    /// Breaks all lines with the specified maximum advance.
    pub fn break_all_lines(&mut self, max_advance: Option<f32>) {
        self.break_lines()
            .break_remaining(max_advance.unwrap_or(f32::MAX));
    }

    /// Apply alignment to the layout.
    ///
    /// You must perform line breaking prior to aligning, through [`Layout::break_lines`] or
    /// [`Layout::break_all_lines`].
    ///
    /// If a finite `max_advance` is supplied to `Layout::break_all_lines` then that width will be applied
    /// relative to that width. Otherwise alignment will be applied relative to the width of the
    /// longest line as computed by line breaking.
    ///
    /// If line-specific `offset` and `max_advance` are set using the advanced methods on the `BreakLines`
    /// struct then each line will be aligned individually within its line box.
    pub fn align(&mut self, alignment: Alignment, options: AlignmentOptions) {
        unjustify(&mut self.data);
        align(&mut self.data, alignment, options);
    }

    /// Returns the index and `Line` object for the line containing the
    /// given byte `index` in the source text.
    pub(crate) fn line_for_byte_index(&self, index: usize) -> Option<(usize, Line<'_, B>)> {
        let line_index = self
            .data
            .lines
            .binary_search_by(|line| {
                if index < line.text_range.start {
                    Ordering::Greater
                } else if index >= line.text_range.end {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .ok()?;
        Some((line_index, self.get(line_index)?))
    }

    /// Returns the index and `Line` object for the line containing the
    /// given `offset`.
    ///
    /// The offset is specified in the direction orthogonal to line direction.
    /// For horizontal text, this is a vertical or y offset. If the offset is
    /// on a line boundary, it is considered to be contained by the later line.
    pub(crate) fn line_for_offset(&self, offset: f32) -> Option<(usize, Line<'_, B>)> {
        if offset < 0.0 {
            return Some((0, self.get(0)?));
        }
        let maybe_line_index = self.data.lines.binary_search_by(|line| {
            if offset < line.metrics.block_min_coord {
                Ordering::Greater
            } else if offset >= line.metrics.block_max_coord {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        });
        let line_index = match maybe_line_index {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        };
        Some((line_index, self.get(line_index)?))
    }
}

impl<B: Brush> Default for Layout<B> {
    fn default() -> Self {
        Self {
            data: LayoutData::default(),
        }
    }
}
