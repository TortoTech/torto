//! Style resolution from authored Reading IR into reader-controlled metrics.
//!
//! This module deliberately stops before shaping or pagination. Its output is
//! still Reading IR, but every metric that affects flow has been resolved.

use std::borrow::Cow;

use rebook_publication::{
    Inline, Rgba, TextAlignment, TextBaseline, TextBlock, TextBlockKind, WritingSystem,
};

use crate::{ParagraphIndentMode, ReaderStyle, ReaderTypesetting, TypesettingMode};

const BOOK_TABLE_BLOCK_GAP: f32 = 14.0;

/// Placement context used while resolving one text block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextContext {
    Flow,
    Table,
}

/// Resolved metrics shared by table measurement and final table layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResolvedTableMetrics {
    pub font_scale: f32,
    pub line_height: f32,
    pub cell_padding: f32,
    pub block_gap: f32,
}

#[must_use]
pub(crate) fn resolve_table_metrics(reader_style: &ReaderStyle) -> ResolvedTableMetrics {
    if reader_style.typesetting.mode == TypesettingMode::Unified {
        ResolvedTableMetrics {
            font_scale: reader_style.typesetting.table_font_scale,
            line_height: reader_style.typesetting.table_line_height,
            cell_padding: reader_style.typography.font_size
                * reader_style.typesetting.table_cell_padding_em,
            block_gap: reader_style.typography.font_size * 0.7,
        }
    } else {
        ResolvedTableMetrics {
            font_scale: 1.0,
            line_height: 1.3,
            cell_padding: 6.0,
            block_gap: BOOK_TABLE_BLOCK_GAP,
        }
    }
}

/// Resolves authored block styling against the reader's current typesetting
/// profile without shaping text or choosing pagination breakpoints.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the style cascade is kept as one deterministic authored-to-resolved transformation"
)]
pub(crate) fn resolve_text_block<'a>(
    block: &'a TextBlock,
    reader_style: &ReaderStyle,
    context: TextContext,
) -> Cow<'a, TextBlock> {
    if reader_style.typesetting.mode != TypesettingMode::Unified {
        if block.style.hard_break_after {
            let mut resolved = block.clone();
            resolved.style.margin_after +=
                reader_style.typography.font_size * resolved.style.line_height.max(1.0);
            return Cow::Owned(resolved);
        }
        return Cow::Borrowed(block);
    }

    let mut resolved = block.clone();
    let typography = &reader_style.typography;
    let profile = &reader_style.typesetting;
    let base_size = typography.font_size;
    let (scale, line_height, margin_after) = match context {
        TextContext::Table => (profile.table_font_scale, profile.table_line_height, 0.0),
        TextContext::Flow => match block.kind {
            TextBlockKind::Heading(level) => (
                unified_heading_scale(profile.heading_scale, level),
                1.3,
                base_size * profile.heading_body_gap_em,
            ),
            TextBlockKind::Caption => (profile.caption_font_scale, 1.4, 0.0),
            TextBlockKind::Preformatted => (0.9, 1.45, base_size * profile.paragraph_gap_em),
            TextBlockKind::Blockquote => (
                0.95,
                profile.body_line_height,
                base_size * profile.paragraph_gap_em,
            ),
            TextBlockKind::QuoteAttribution => (0.88, 1.4, 0.0),
            TextBlockKind::Paragraph
            | TextBlockKind::ListItem { .. }
            | TextBlockKind::DefinitionDescription { .. } => (
                1.0,
                profile.body_line_height,
                base_size * profile.paragraph_gap_em,
            ),
            TextBlockKind::DefinitionTerm { .. } => (
                1.0,
                profile.body_line_height,
                base_size * profile.paragraph_gap_em.min(0.25),
            ),
        },
    };
    let margin_after = margin_after
        + if block.style.hard_break_after {
            base_size * line_height
        } else {
            0.0
        };

    if context == TextContext::Flow && block.kind != TextBlockKind::Blockquote {
        resolved.style.align = if matches!(
            block.kind,
            TextBlockKind::Paragraph | TextBlockKind::ListItem { .. }
        ) {
            TextAlignment::Justify
        } else {
            TextAlignment::Start
        };
    }
    resolved.style.margin_before = 0.0;
    resolved.style.margin_after = margin_after;
    resolved.style.indent = if context == TextContext::Flow {
        match block.kind {
            TextBlockKind::Paragraph => {
                base_size * paragraph_indent_em(profile, reader_style.writing_system)
            }
            TextBlockKind::Blockquote => block.style.indent,
            _ => 0.0,
        }
    } else {
        0.0
    };
    resolved.style.line_height = line_height;
    match context {
        TextContext::Table => {
            resolved.style.margin_start = 0.0;
            resolved.style.margin_start_fraction = 0.0;
        }
        TextContext::Flow => match block.kind {
            TextBlockKind::Caption => {
                resolved.style.align = TextAlignment::Center;
                resolved.style.margin_start = 0.0;
                resolved.style.margin_start_fraction = 0.0;
            }
            TextBlockKind::ListItem { depth, .. }
            | TextBlockKind::DefinitionDescription { depth } => {
                resolved.style.margin_start =
                    base_size * profile.list_indent_em * (f32::from(depth) + 1.0);
                resolved.style.margin_start_fraction = 0.0;
            }
            TextBlockKind::DefinitionTerm { depth } => {
                resolved.style.margin_start = base_size * profile.list_indent_em * f32::from(depth);
                resolved.style.margin_start_fraction = 0.0;
            }
            TextBlockKind::QuoteAttribution => {
                resolved.style.align = TextAlignment::End;
                resolved.style.margin_start = 0.0;
                resolved.style.margin_start_fraction = 0.0;
            }
            _ => {
                resolved.style.margin_start = 0.0;
                resolved.style.margin_start_fraction = 0.0;
            }
        },
    }

    for inline in &mut resolved.content {
        match inline {
            Inline::Text(run) => {
                run.style.color = Rgba::BLACK;
                run.style.underline = false;
                run.style.size_scale = if run.style.baseline == TextBaseline::Normal {
                    scale
                } else {
                    scale * 0.75
                };
                if matches!(
                    block.kind,
                    TextBlockKind::Heading(_) | TextBlockKind::DefinitionTerm { .. }
                ) {
                    run.style.bold = true;
                }
                if matches!(block.kind, TextBlockKind::Heading(_)) {
                    resolved_inline_heading(run);
                }
                if matches!(
                    block.kind,
                    TextBlockKind::Blockquote | TextBlockKind::QuoteAttribution
                ) {
                    run.style.bold = false;
                    run.style.italic = false;
                }
            }
            Inline::Math(run) => run.size_scale = scale,
            Inline::Break => {}
        }
    }
    Cow::Owned(resolved)
}

fn resolved_inline_heading(run: &mut rebook_publication::TextRun) {
    // Unified/focus typesetting owns heading presentation. Preserve prose
    // emphasis, but not an authored block-level italic heading.
    run.style.italic = false;
}

#[must_use]
pub(crate) fn paragraph_indent_em(
    profile: &ReaderTypesetting,
    writing_system: WritingSystem,
) -> f32 {
    if profile.paragraph_indent_mode == ParagraphIndentMode::Custom {
        return profile.paragraph_indent_em;
    }

    match writing_system {
        WritingSystem::Cjk => 2.0,
        WritingSystem::Latin => 1.5,
        WritingSystem::Other | WritingSystem::Unknown => profile.paragraph_indent_em,
    }
}

fn unified_heading_scale(h1_scale: f32, level: u8) -> f32 {
    let emphasis = (h1_scale - 1.0).max(0.0);
    1.0 + emphasis
        * match level {
            1 => 1.0,
            2 => 0.72,
            3 => 0.45,
            4 => 0.25,
            5 => 0.12,
            _ => 0.05,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{BlockStyle, TextRun, TextStyle};

    fn paragraph() -> TextBlock {
        TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "test".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: BlockStyle::default(),
            source: None,
        }
    }

    #[test]
    fn unified_latin_prose_resolves_to_justified_flow() {
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            writing_system: WritingSystem::Latin,
            ..ReaderStyle::default()
        };
        let resolved = resolve_text_block(&paragraph(), &style, TextContext::Flow).into_owned();
        assert_eq!(resolved.style.align, TextAlignment::Justify);
        assert!((resolved.style.indent - style.typography.font_size * 1.5).abs() < f32::EPSILON);
        assert!((resolved.style.line_height - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn book_mode_keeps_authored_block_borrowed() {
        let block = paragraph();
        assert!(matches!(
            resolve_text_block(&block, &ReaderStyle::default(), TextContext::Flow),
            Cow::Borrowed(_)
        ));
    }
}
