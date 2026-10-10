//! Re-linebreak plain cells without repeating width-independent shaping.
use crate::{
    Alignment, AlignmentOptions, Arc, Inline, InlineRole, Layout, LayoutEngine, LinkRole,
    PreparedText, ReaderStyle, TextBlock, TextBlockKind, TextBrush, TypesettingMode,
    break_text_lines, empty_text_source_spans, resolve_text_measure, text_alignment,
};

pub(super) fn reusable_cell(block: &TextBlock) -> bool {
    block.kind == TextBlockKind::Paragraph
        && !block.style.sentence_indents
        && !block.style.preserve_sentence_prefix
        && block.content.iter().all(|inline| match inline {
            Inline::Text(run) => {
                run.link.is_none()
                    && run.style.inline_citation == 0
                    && run.style.footnote_number == 0
                    && run.style.link_role == LinkRole::Normal
                    && run.style.inline_role == InlineRole::Normal
                    && run.style.display_writing_system.is_none()
            }
            Inline::Break => true,
            // Ruby, formulas, icons and note markers have width-dependent
            // preparation or paint bounds; keep their existing shaping path.
            _ => false,
        })
}

pub(super) fn reusable_measurement(block: &TextBlock, measured: &PreparedText) -> bool {
    reusable_cell(block)
        // A naturally wrapped measurement may already have adjusted cluster
        // advances for justification. Do not round-trip those adjustments.
        && measured.layout.lines().all(|line| {
            matches!(line.break_reason(), parley::layout::BreakReason::None | parley::layout::BreakReason::Explicit)
        })
}

impl LayoutEngine {
    pub(super) fn empty_table_cell(
        block: &TextBlock,
        reader_style: &ReaderStyle,
        content_width: f32,
        minimum_width: f32,
    ) -> Option<PreparedText> {
        if reader_style.typesetting.mode != TypesettingMode::Unified
            || !reusable_cell(block)
            || !block
                .content
                .iter()
                .all(|inline| matches!(inline, Inline::Text(run) if run.text.is_empty()))
        {
            return None;
        }
        // Whitespace and forced breaks are intentionally not empty. Source
        // ownership remains on PreparedTableCell even when there is no text.
        let (start_offset, available_width, _) =
            resolve_text_measure(block, content_width, minimum_width);
        Some(PreparedText {
            source_spans: empty_text_source_spans(),
            ruby: Arc::from([]),
            citations: Arc::from([]),
            layout: Arc::new(Layout::new()),
            lines: 0..0,
            text: Arc::from(""),
            source_text_start: 0,
            start_offset,
            available_width,
            inline_images: Arc::from([]),
            hyphens: Arc::from([]),
        })
    }

    pub(super) fn reflow_table_cell(
        mut measured: PreparedText,
        block: &TextBlock,
        content_width: f32,
        minimum_width: f32,
    ) -> PreparedText {
        let (start_offset, available_width, _) =
            resolve_text_measure(block, content_width, minimum_width);
        let layout = Arc::make_mut(&mut measured.layout);
        if !measured.text.is_empty() {
            // Parley clears prior justification and rebuilds line geometry.
            // Shaped glyphs, styles, bidi analysis and source offsets survive.
            break_text_lines(layout, Some(available_width));
            layout.align(
                resolved_text_alignment(layout, block, false),
                AlignmentOptions::default(),
            );
        }
        measured.lines = 0..layout.len();
        measured.start_offset = start_offset;
        measured.available_width = available_width;
        measured
    }
}

pub(super) fn resolved_text_alignment(
    layout: &Layout<TextBrush>,
    block: &TextBlock,
    optimized: bool,
) -> Alignment {
    if optimized {
        return Alignment::Start;
    }
    let alignment = text_alignment(block.style.align);
    if layout.is_rtl() && !block.style.logical_alignment {
        match alignment {
            Alignment::Start => Alignment::End,
            Alignment::End => Alignment::Start,
            other => other,
        }
    } else {
        alignment
    }
}
