//! Optical note spacing measured from the actual fallback font and shaped glyphs.
use super::{StyledRange, TextBrush, linebreak::parley::SpacingAdjustment};
use parley::Layout;
use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    instance::{LocationRef, NormalizedCoord, Size},
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

fn cjk_text(ch: char) -> bool {
    matches!(ch, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}'
        | '\u{3040}'..='\u{30ff}' | '\u{ac00}'..='\u{d7af}')
}
fn closing_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '\u{3002}'
            | '\u{ff01}'
            | '\u{ff1f}'
            | '\u{ff0c}'
            | '\u{3001}'
            | '\u{ff1b}'
            | '\u{ff1a}'
            | '\u{ff09}'
            | '\u{300d}'
            | '\u{300f}'
            | '\u{3011}'
    )
}
fn notes(spans: &[StyledRange]) -> impl Iterator<Item = &StyledRange> {
    spans
        .iter()
        .filter(|span| span.footnote_reference_group & 0x2000_0000 != 0)
}
pub(super) fn needs_measurement(text: &str, spans: &[StyledRange]) -> bool {
    notes(spans).any(|span| {
        text[..span.range.start]
            .chars()
            .next_back()
            .is_some_and(closing_punctuation)
            || text[span.range.end..].chars().next().is_some_and(cjk_text)
    })
}
#[derive(Clone, Copy, Debug)]
struct Ink {
    left: f32,
    right: f32,
    advance: f32,
}

fn compression(previous: Ink, note: Ink, em: f32) -> f32 {
    let gap = previous.advance - previous.right + note.left;
    // Keep a visible gap, and retain at least 20% of the punctuation's advance.
    (em * 0.10 - gap).clamp(-(em * 0.65).min(previous.advance * 0.8), 0.0)
}
fn separation(note: Ink, next: Ink, em: f32) -> f32 {
    let gap = note.advance - note.right + next.left;
    (em * 0.16 - gap).clamp(0.0, em * 0.20)
}
pub(super) fn measure(
    layout: &Layout<TextBrush>,
    text: &str,
    spans: &[StyledRange],
    em: f32,
) -> Vec<SpacingAdjustment> {
    let mut offsets = HashSet::new();
    for span in notes(spans) {
        if let Some((offset, ch)) = text[..span.range.start].char_indices().next_back()
            && closing_punctuation(ch)
        {
            offsets.insert(offset);
            offsets.insert(span.range.start);
        }
        if text[span.range.end..].chars().next().is_some_and(cjk_text) {
            offsets.insert(span.range.end);
            if let Some((offset, _)) = text[span.range.clone()]
                .char_indices()
                .rev()
                .find(|(_, ch)| ch.is_ascii_digit())
            {
                offsets.insert(span.range.start + offset);
            }
        }
    }
    let mut ink: HashMap<usize, (Range<usize>, Ink)> = HashMap::new();
    for line in layout.lines() {
        for run in line.runs() {
            if run.is_rtl()
                || !offsets
                    .iter()
                    .any(|offset| run.text_range().contains(offset))
            {
                continue;
            }
            let Ok(font) = FontRef::from_index(run.font().data.as_ref(), run.font().index) else {
                continue;
            };
            let coords: Vec<_> = run
                .normalized_coords()
                .iter()
                .map(|coord| NormalizedCoord::from_bits(*coord))
                .collect();
            let metrics = font.glyph_metrics(Size::new(run.font_size()), LocationRef::new(&coords));
            for cluster in run.clusters() {
                let range = cluster.text_range();
                if !offsets.contains(&range.start) {
                    continue;
                }
                let mut pen = 0.0_f32;
                let mut left = f32::INFINITY;
                let mut right = f32::NEG_INFINITY;
                for glyph in cluster.glyphs() {
                    if let Some(bounds) = metrics.bounds(GlyphId::new(glyph.id)) {
                        left = left.min(pen + glyph.x + bounds.x_min);
                        right = right.max(pen + glyph.x + bounds.x_max);
                    }
                    pen += glyph.advance;
                }
                if left.is_finite() && right.is_finite() {
                    ink.insert(
                        range.start,
                        (
                            range,
                            Ink {
                                left,
                                right,
                                advance: cluster.advance(),
                            },
                        ),
                    );
                }
            }
        }
    }
    let mut result = Vec::new();
    for span in notes(spans) {
        if let Some((offset, ch)) = text[..span.range.start].char_indices().next_back()
            && closing_punctuation(ch)
            && let (Some((range, previous)), Some((_, note))) =
                (ink.get(&offset), ink.get(&span.range.start))
        {
            let amount = compression(*previous, *note, em);
            if amount < -0.001 {
                result.push(SpacingAdjustment {
                    range: range.clone(),
                    amount,
                });
            }
        }
        if text[span.range.end..].chars().next().is_some_and(cjk_text)
            && let Some((offset, _)) = text[span.range.clone()]
                .char_indices()
                .rev()
                .find(|(_, ch)| ch.is_ascii_digit())
            && let (Some((range, note)), Some((_, next))) = (
                ink.get(&(span.range.start + offset)),
                ink.get(&span.range.end),
            )
        {
            let amount = separation(*note, *next, em);
            if amount > 0.001 {
                result.push(SpacingAdjustment {
                    range: range.clone(),
                    amount,
                });
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shaped_cjk_notes_reserve_optical_spacing_and_keep_source_offsets() {
        use super::super::{
            LayoutEngine, ReaderFontBlob, ReaderTypography, Rgba, TextBaseline, TextStyle,
        };
        use std::sync::Arc;
        let mut engine = LayoutEngine::with_fonts([
            ReaderFontBlob::new(Arc::new(
                include_bytes!("../../../assets/fonts/LXGWWenKaiGBScreen.ttf").as_slice(),
            )),
            ReaderFontBlob::new(Arc::new(
                include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice(),
            )),
        ]);
        let typography = ReaderTypography::default();
        for label in ["1", "12"] {
            let text =
                format!("\u{53e5}\u{5b50}\u{3002}{label}\u{2060}\u{4e0b}\u{4e00}\u{53e5}\u{3002}");
            let start = text.find(label).unwrap();
            let end = start + label.len() + '\u{2060}'.len_utf8();
            let spans = [
                StyledRange {
                    ruby: None,
                    range: 0..start,
                    style: TextStyle::default(),
                    footnote_reference_group: 0,
                    hyphenation_suppressed: false,
                },
                StyledRange {
                    ruby: None,
                    range: start..end,
                    style: TextStyle {
                        size_scale: 0.78,
                        baseline: TextBaseline::Superscript,
                        ..Default::default()
                    },
                    footnote_reference_group: 0x2000_0001,
                    hyphenation_suppressed: true,
                },
                StyledRange {
                    ruby: None,
                    range: end..text.len(),
                    style: TextStyle::default(),
                    footnote_reference_group: 0,
                    hyphenation_suppressed: false,
                },
            ];
            let mut raw = engine.build_text_layout_raw(
                &text,
                &spans,
                &[],
                "Literata, LXGW WenKai GB Screen",
                &typography,
                1.5,
                Rgba::BLACK,
                &[],
                &[],
                &[],
                rebook_publication::TextDirection::Auto,
            );
            raw.break_all_lines(None);
            let measured = measure(&raw, &text, &spans, typography.font_size);
            assert!(
                measured
                    .iter()
                    .any(|item| item.range.end == start && item.amount < -1.0),
                "{measured:?}"
            );
            assert!(
                measured
                    .iter()
                    .all(|item| !text[item.range.clone()].contains('\u{2060}'))
            );
            let mut adjusted = engine.build_text_layout(
                &text,
                &spans,
                &[],
                "Literata, LXGW WenKai GB Screen",
                &typography,
                1.5,
                Rgba::BLACK,
                &[],
                rebook_publication::TextDirection::Auto,
            );
            adjusted.break_all_lines(None);
            let delta: f32 = measured.iter().map(|item| item.amount).sum();
            assert!((adjusted.width() - raw.width() - delta).abs() < 0.05);
            assert_eq!(adjusted.get(0).unwrap().text_range(), 0..text.len());
            for width in [40.0, 65.0, 100.0] {
                adjusted.break_all_lines(Some(width));
                super::super::linebreak::parley::repair_trailing_footnote_line(
                    &mut adjusted,
                    &text,
                    width,
                );
                assert!(adjusted.lines().all(|line| line.runs().any(|run| {
                    run.clusters()
                        .any(|cluster| !cluster.first_style().brush.footnote_reference)
                })));
            }
        }
        for text in ["Sentence.1Next", "Sentence.1 Next", "Sentence\u{201d}1Next"] {
            let start = text.find('1').unwrap();
            let spans = [StyledRange {
                ruby: None,
                range: start..start + 1,
                style: TextStyle::default(),
                footnote_reference_group: 0x2000_0001,
                hyphenation_suppressed: true,
            }];
            assert!(!needs_measurement(text, &spans));
        }
    }

    #[test]
    fn optical_adjustment_uses_ink_not_full_width() {
        let note = Ink {
            left: 0.4,
            right: 5.5,
            advance: 6.0,
        };
        let wide = Ink {
            left: 1.0,
            right: 6.0,
            advance: 21.6,
        };
        let compact = Ink {
            right: 19.6,
            ..wide
        };
        assert!(compression(wide, note, 20.0) < -10.0);
        assert!(compression(compact, note, 20.0) > -1.0);
        let han = Ink {
            left: 0.5,
            right: 19.0,
            advance: 20.0,
        };
        assert!((separation(note, han, 20.0) - 2.2).abs() < 0.001);
        assert_eq!(
            separation(
                Ink {
                    advance: 10.0,
                    ..note
                },
                han,
                20.0
            ),
            0.0
        );
        assert!(compression(wide, note, 20.0) >= -13.0);
    }
}
