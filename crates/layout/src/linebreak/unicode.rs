//! Backend-neutral Unicode line-break candidates and coarse script gating.
//!
//! This module owns the UAX #14 decision boundary. Shaping adapters may map
//! these UTF-8 byte positions to their clusters, but must not substitute a
//! backend-specific word-boundary flag for the authored text decision.

use unicode_linebreak::{BreakOpportunity as UnicodeBreakOpportunity, linebreaks};

/// One legal line boundary in authored UTF-8 text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineBreakOpportunity {
    /// Byte index of the character following the break.
    pub byte_index: usize,
    pub kind: LineBreakKind,
}

/// UAX #14 break strength retained independently from a shaping backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineBreakKind {
    Allowed,
    Mandatory,
}

/// Coarse phase gate for paragraph algorithms that do not support `BiDi` yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParagraphScript {
    Ltr,
    Cjk,
    /// Left-to-right prose containing both Latin-like and CJK scripts. This
    /// remains suitable for UAX #14 greedy breaking, but not for the current
    /// phase-one space-delimited Knuth–Plass adapter.
    LtrCjk,
    Rtl,
    MixedOrUnsupported,
}

/// Returns all UAX #14 opportunities as UTF-8 byte positions.
#[must_use]
pub fn opportunities(text: &str) -> Vec<LineBreakOpportunity> {
    linebreaks(text)
        .map(|(byte_index, opportunity)| LineBreakOpportunity {
            byte_index,
            kind: match opportunity {
                UnicodeBreakOpportunity::Allowed => LineBreakKind::Allowed,
                UnicodeBreakOpportunity::Mandatory => LineBreakKind::Mandatory,
            },
        })
        .collect()
}

/// Returns whether UAX #14 permits a soft or mandatory break at `byte_index`.
#[must_use]
pub fn contains(opportunities: &[LineBreakOpportunity], byte_index: usize) -> bool {
    opportunities
        .binary_search_by_key(&byte_index, |opportunity| opportunity.byte_index)
        .is_ok()
}

/// Classifies the scripts relevant to the staged line-breaking migration.
///
/// This is intentionally conservative: ordinary punctuation and digits inherit
/// the surrounding class, while controls and non-space whitespace remain
/// unsupported by the first optimized LTR adapter.
#[must_use]
pub fn classify_paragraph(text: &str) -> ParagraphScript {
    let mut saw_ltr = false;
    let mut saw_cjk = false;
    let mut saw_rtl = false;
    for character in text.chars() {
        if character.is_control() || character.is_whitespace() {
            // NBSP is common between a synthetic list marker and authored
            // content. It participates in shaping but never becomes a legal
            // UAX break, so it is safe for the non-BiDi native path.
            if matches!(character, ' ' | '\u{00A0}') {
                continue;
            }
            return ParagraphScript::MixedOrUnsupported;
        }
        if character.is_ascii_punctuation()
            || character.is_numeric()
            || is_neutral_punctuation(character)
        {
            continue;
        }
        if is_cjk(character) {
            saw_cjk = true;
        } else if is_rtl_codepoint(character) {
            saw_rtl = true;
        } else {
            saw_ltr = true;
        }
    }
    match (saw_ltr, saw_cjk, saw_rtl) {
        (true | false, false, false) => ParagraphScript::Ltr,
        (false, true, false) => ParagraphScript::Cjk,
        (true, true, false) => ParagraphScript::LtrCjk,
        (false, false, true) => ParagraphScript::Rtl,
        _ => ParagraphScript::MixedOrUnsupported,
    }
}

fn is_neutral_punctuation(character: char) -> bool {
    matches!(
        character as u32,
        0x200B..=0x206F
            | 0x2E00..=0x2E7F
            | 0x3001..=0x303F
            | 0xFE10..=0xFE1F
            | 0xFE30..=0xFE6F
            | 0xFF01..=0xFF0F
            | 0xFF1A..=0xFF20
            | 0xFF3B..=0xFF40
            | 0xFF5B..=0xFF65
    )
}

/// Phase-one Knuth–Plass currently accepts only ordinary LTR prose whose legal
/// internal boundaries are ordinary ASCII spaces.
#[must_use]
pub fn supports_phase_one_optimized(text: &str) -> bool {
    classify_paragraph(text) == ParagraphScript::Ltr
        && text
            .chars()
            .all(|character| character == ' ' || !character.is_whitespace())
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x2E80..=0x2FFF
            | 0x3040..=0x30FF
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0x20000..=0x323AF
    )
}

fn is_rtl_codepoint(character: char) -> bool {
    matches!(
        character as u32,
        0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x1EE00..=0x1EEFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uax14_candidates_are_byte_aligned_and_keep_mandatory_end() {
        let text = "Hello 世界";
        let breaks = opportunities(text);
        assert!(
            breaks
                .iter()
                .all(|item| text.is_char_boundary(item.byte_index))
        );
        assert!(contains(&breaks, "Hello ".len()));
        assert_eq!(
            breaks.last(),
            Some(&LineBreakOpportunity {
                byte_index: text.len(),
                kind: LineBreakKind::Mandatory,
            })
        );
    }

    #[test]
    fn uax14_does_not_break_before_closing_punctuation() {
        let text = "甲，乙";
        let breaks = opportunities(text);
        let comma_start = '甲'.len_utf8();
        assert!(!contains(&breaks, comma_start));
        assert!(contains(&breaks, comma_start + '，'.len_utf8()));
    }

    #[test]
    fn phase_gate_distinguishes_ltr_cjk_ltr_cjk_and_rtl_text() {
        assert_eq!(classify_paragraph("Ordinary prose."), ParagraphScript::Ltr);
        assert_eq!(classify_paragraph("普通正文"), ParagraphScript::Cjk);
        assert_eq!(classify_paragraph("普通正文。"), ParagraphScript::Cjk);
        assert_eq!(
            classify_paragraph("•\u{00A0}List item"),
            ParagraphScript::Ltr
        );
        assert_eq!(classify_paragraph("مرحبا"), ParagraphScript::Rtl);
        assert_eq!(classify_paragraph("Latin 中文"), ParagraphScript::LtrCjk);
        assert_eq!(
            classify_paragraph("Latin مرحبا 中文"),
            ParagraphScript::MixedOrUnsupported
        );
        assert!(supports_phase_one_optimized("Ordinary English prose."));
        assert!(!supports_phase_one_optimized("two\tcolumns"));
    }
}
