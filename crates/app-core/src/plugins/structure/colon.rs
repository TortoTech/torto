//! Decide whether an added colon break separates a complete single sentence.
//! This is punctuation-based: content length never affects the decision.
use super::{ParagraphAtom, paired_closer, paired_opener, sentence_char_ranges};
use std::ops::Range;

pub(super) fn filter_boundaries(
    chars: &[char],
    protected: &[Range<usize>],
    atoms: &[ParagraphAtom],
    language_hint: &str,
    mut boundaries: Vec<usize>,
) -> Vec<usize> {
    if boundaries.is_empty() {
        return boundaries;
    }
    let mut mask = vec![false; chars.len()];
    for range in protected {
        for value in &mut mask[range.start.min(chars.len())..range.end.min(chars.len())] {
            *value = true;
        }
    }
    let periods = sentence_periods(chars, &mask, atoms, language_hint);
    boundaries.retain(|end| {
        let whitespace_start = chars[..*end]
            .iter()
            .rposition(|ch| !ch.is_whitespace())
            .map_or(0, |index| index + 1);
        chars[whitespace_start..*end]
            .iter()
            .any(|ch| matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            || !continues_single_sentence(chars, *end, &mask, &periods)
    });
    boundaries
}

fn ascii_quote(chars: &[char], index: usize) -> bool {
    let ch = chars[index];
    if !matches!(ch, '\'' | '"') {
        return false;
    }
    let escaped = chars[..index]
        .iter()
        .rev()
        .take_while(|c| **c == '\\')
        .count()
        % 2
        == 1;
    let apostrophe = ch == '\''
        && index
            .checked_sub(1)
            .is_some_and(|i| chars[i].is_alphanumeric())
        && chars.get(index + 1).is_some_and(|c| c.is_alphanumeric());
    !escaped && !apostrophe
}

fn is_delimiter(chars: &[char], index: usize) -> bool {
    paired_closer(chars[index]).is_some()
        || paired_opener(chars[index]).is_some()
        || ascii_quote(chars, index)
}

/// Reuse ordinary sentence ends for periods (not decimals/abbreviations).
/// SentenceX groups an entire quoted passage. When periods occur inside paired
/// text, neutralize only delimiters in a diagnostic copy to reveal its sentence
/// ends. Scalar offsets and every source character in the real view stay intact.
fn sentence_periods(
    chars: &[char],
    protected: &[bool],
    atoms: &[ParagraphAtom],
    language_hint: &str,
) -> Vec<bool> {
    let mut periods = vec![false; chars.len()];
    if !chars.contains(&'.') {
        return periods;
    }
    let ends: Vec<usize> = if (0..chars.len()).any(|i| !protected[i] && is_delimiter(chars, i)) {
        let plain: String = chars
            .iter()
            .enumerate()
            .map(|(i, ch)| {
                if protected[i] {
                    '\u{fffc}'
                } else if is_delimiter(chars, i) || ch.is_whitespace() && !matches!(ch, '\n' | '\r')
                {
                    ' '
                } else {
                    *ch
                }
            })
            .collect();
        sentence_char_ranges(&plain, language_hint)
            .into_iter()
            .map(|range| range.end)
            .collect()
    } else {
        atoms.iter().map(|atom| atom.end).collect()
    };
    for end in ends {
        let mut cursor = end.min(chars.len());
        while cursor > 0
            && (protected[cursor - 1]
                || chars[cursor - 1].is_whitespace()
                || paired_opener(chars[cursor - 1]).is_some()
                || ascii_quote(chars, cursor - 1))
        {
            cursor -= 1;
        }
        while cursor > 0 && matches!(chars[cursor - 1], '.' | '!' | '?' | '。' | '！' | '？') {
            cursor -= 1;
            // ASCII ellipses are not a complete-sentence signal for this rule.
            if chars[cursor] == '.'
                && chars.get(cursor + 1) != Some(&'.')
                && cursor.checked_sub(1).is_none_or(|i| chars[i] != '.')
            {
                periods[cursor] = true;
            }
        }
    }
    periods
}

fn continues_single_sentence(
    chars: &[char],
    start: usize,
    protected: &[bool],
    periods: &[bool],
) -> bool {
    let mut closers = Vec::new();
    let mut content = false;
    let mut ended = false;
    for index in start..chars.len() {
        let ch = chars[index];
        if protected[index] {
            continue;
        }
        if matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            return false;
        }
        if ch.is_whitespace() {
            continue;
        }
        if closers.last() == Some(&ch) {
            closers.pop();
            if ended && closers.is_empty() {
                return true;
            }
            continue;
        }
        if ended {
            if matches!(ch, '。' | '！' | '？' | '.' | '!' | '?') {
                continue;
            }
            // A second sentence/continuation before the closing delimiter keeps
            // the colon break, even if the first quoted sentence is tiny.
            return false;
        }
        if let Some(closer) = paired_closer(ch).or_else(|| ascii_quote(chars, index).then_some(ch))
        {
            closers.push(closer);
            continue;
        }
        if matches!(ch, '，' | ',' | '、' | '；' | ';' | '：' | ':' | '…') {
            return false;
        }
        if ch == '.'
            && (chars.get(index + 1) == Some(&'.')
                || index.checked_sub(1).is_some_and(|i| chars[i] == '.'))
        {
            return false;
        }
        if matches!(ch, '。' | '！' | '？' | '!' | '?') || ch == '.' && periods[index] {
            if !content {
                return false;
            }
            if closers.is_empty() {
                return true;
            }
            ended = true;
        } else {
            content |= ch.is_alphanumeric();
        }
    }
    false
}

#[cfg(test)]
#[path = "colon/tests.rs"]
mod tests;
