//! Application-level structural protection before the multilingual segmenter.
use std::borrow::Cow;
use std::ops::Range;

pub(super) fn abbreviation_ranges(text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    for (dot, _) in text.char_indices().filter(|(_, c)| *c == '.') {
        let mut start = dot;
        for (byte, c) in text[..dot].char_indices().rev().take(16) {
            if !c.is_ascii_alphabetic() && c != '.' {
                break;
            }
            start = byte;
        }
        let word = text[start..dot].to_ascii_lowercase();
        let mut following = text[dot + 1..].char_indices();
        let Some((at, next)) = following.find(|(_, c)| {
            !c.is_whitespace() || matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
        }) else {
            continue;
        };
        let protects = match word.as_str() {
            "mr" | "mrs" | "ms" | "dr" | "prof" | "st" => next.is_alphabetic(),
            "fig" | "figs" | "eq" | "eqs" | "no" | "nos" | "vol" | "pp" | "p" => {
                next.is_ascii_digit()
            }
            "e.g" | "i.e" | "cf" | "vs" => next.is_alphanumeric(),
            _ => false,
        };
        if protects {
            spans.push(start..dot + 1 + at + next.len_utf8());
        }
    }
    spans
}

pub(super) fn segmentation_text(text: &str) -> Cow<'_, str> {
    if !text.bytes().any(|c| c.is_ascii_digit()) && !text.contains(['《', '〈']) {
        return Cow::Borrowed(text);
    }
    let chars = text.char_indices().collect::<Vec<_>>();
    let mut previous = None;
    let mut line_start = true;
    let contexts = chars
        .iter()
        .map(|&(_, c)| {
            let allowed = line_start || previous.is_some_and(|p| "。！？.!?:：;；".contains(p));
            if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                line_start = true;
            } else if !c.is_whitespace() {
                previous = Some(c);
                line_start = false;
            }
            allowed
        })
        .collect::<Vec<_>>();
    let mut result = text.as_bytes().to_vec();
    let mut changed = false;
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        // A decimal/version component is not a new enumerator merely because
        // the previous scalar is a period (e.g. the 14 in "3.14. Next").
        if start >= 2 && chars[start - 1].1 == '.' && chars[start - 2].1.is_ascii_digit() {
            i += 1;
            continue;
        }
        if !contexts[start] {
            i += 1;
            continue;
        }
        let closer = match chars[i].1 {
            '(' => Some(')'),
            '[' => Some(']'),
            '（' => Some('）'),
            '【' => Some('】'),
            _ => None,
        };
        let digits = i + usize::from(closer.is_some());
        let mut end = digits;
        while end < chars.len() && chars[end].1.is_ascii_digit() {
            end += 1;
        }
        if end == digits || end - digits > 3 {
            i += 1;
            continue;
        }
        let Some(&(_, mark)) = chars.get(end) else {
            break;
        };
        if !closer.map_or_else(|| ".)）、．".contains(mark), |c| c == mark) {
            i += 1;
            continue;
        }
        if chars.get(end + 1).is_some_and(|(_, c)| c.is_ascii_digit()) {
            i = end + 1;
            continue;
        }
        if !matches!(mark, '.' | '．')
            && !chars.get(end + 1).is_some_and(|(_, c)| c.is_whitespace())
        {
            i = end + 1;
            continue;
        }
        if !chars[end + 1..].iter().any(|(_, c)| !c.is_whitespace()) {
            i = end + 1;
            continue;
        }
        for &(byte, c) in &chars[start..=end] {
            // Equal UTF-8 lengths keep every source byte offset unchanged.
            let replacement = if c.len_utf8() == 1 { "X" } else { "Ｘ" };
            result[byte..byte + c.len_utf8()].copy_from_slice(replacement.as_bytes());
        }
        changed = true;
        i = end + 1;
    }
    changed |= mask_title_punctuation(&chars, &mut result);
    if changed {
        Cow::Owned(String::from_utf8(result).expect("equal-width Unicode masks"))
    } else {
        Cow::Borrowed(text)
    }
}

/// A title's internal question mark is not the end of its containing sentence.
/// Mask before segmentation: extending a boundary merely to the closing bracket
/// would still split “在《为什么？》中指出…”. Byte and scalar offsets stay intact.
fn mask_title_punctuation(chars: &[(usize, char)], output: &mut [u8]) -> bool {
    let mut stack = Vec::new();
    let mut coverage = vec![0i32; chars.len() + 1];
    for (index, &(_, ch)) in chars.iter().enumerate() {
        if matches!(ch, '《' | '〈') {
            stack.push((ch, index));
        } else if let Some(&(opener, start)) = stack.last()
            && matches!((opener, ch), ('《', '》') | ('〈', '〉'))
        {
            stack.pop();
            coverage[start] += 1;
            coverage[index + 1] -= 1;
        }
    }
    let mut depth = 0;
    let mut changed = false;
    for (index, &(byte, ch)) in chars.iter().enumerate() {
        depth += coverage[index];
        if depth > 0
            && matches!(
                ch,
                '.' | '!' | '?' | ':' | ';' | '。' | '！' | '？' | '：' | '；'
            )
        {
            let replacement = if ch.len_utf8() == 1 { "X" } else { "文" };
            output[byte..byte + ch.len_utf8()].copy_from_slice(replacement.as_bytes());
            changed = true;
        }
    }
    changed
}

pub(super) fn latin_name_end(chars: &[(usize, char)], start: usize) -> Option<usize> {
    if !chars
        .get(start)
        .is_some_and(|(_, c)| c.is_ascii_uppercase())
    {
        return None;
    }
    let end = chars[start..]
        .iter()
        .position(|(_, c)| !c.is_ascii_alphabetic() && !matches!(c, '-' | '\'' | '’'))
        .map_or(chars.len(), |i| start + i);
    if end - start < 2
        || !chars[start + 1..end]
            .iter()
            .any(|(_, c)| c.is_ascii_lowercase())
    {
        return None;
    }
    let word = chars[start..end]
        .iter()
        .map(|(_, c)| *c)
        .collect::<String>();
    if [
        "Next", "Then", "The", "This", "That", "These", "Those", "It", "But", "However", "He",
        "She", "We", "They", "You", "First", "Second", "Third", "Did", "Do", "Does", "Is", "Are",
    ]
    .contains(&word.as_str())
    {
        return None;
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::super::sentence_byte_ranges_with_language;
    #[test]
    fn book_title_questions_do_not_end_the_containing_sentence() {
        for (text, expected) in [
            (
                "他在《为什么？如何解释？》中指出这个问题。下一句。",
                vec!["他在《为什么？如何解释？》中指出这个问题。", "下一句。"],
            ),
            (
                "请阅读〈What? Why!〉再回答。下一句。",
                vec!["请阅读〈What? Why!〉再回答。", "下一句。"],
            ),
            (
                "这本书是《为什么？》。下一句。",
                vec!["这本书是《为什么？》。", "下一句。"],
            ),
            (
                "他问：“为什么？”下一句。",
                vec!["他问：“为什么？”", "下一句。"],
            ),
        ] {
            let ranges = sentence_byte_ranges_with_language(text, "en");
            assert_eq!(
                ranges.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(),
                expected,
                "{text}"
            );
            assert_eq!(
                ranges.iter().map(|r| &text[r.clone()]).collect::<String>(),
                text
            );
        }
    }
    #[test]
    fn mixed_names_and_enumerators_preserve_real_sentences_and_every_source_byte() {
        for (text, expected) in [
            (
                "作者J. Test提出观点。下一句。",
                vec!["作者J. Test提出观点。", "下一句。"],
            ),
            ("1. 洗涤。2. 晾干。", vec!["1. 洗涤。", "2. 晾干。"]),
            ("1.洗涤。2.晾干。", vec!["1.洗涤。", "2.晾干。"]),
            (
                "1. Wash gently. Dry flat.",
                vec!["1. Wash gently. ", "Dry flat."],
            ),
            (
                "Use 1.5 litres. Next sentence.",
                vec!["Use 1.5 litres. ", "Next sentence."],
            ),
            (
                "It costs 3.14. Next sentence.",
                vec!["It costs 3.14. ", "Next sentence."],
            ),
            (
                "作者Dr. Test介绍Fig. 4.4。下一句。",
                vec!["作者Dr. Test介绍Fig. 4.4。", "下一句。"],
            ),
        ] {
            let ranges = sentence_byte_ranges_with_language(text, "en");
            let actual = ranges.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>();
            assert_eq!(actual, expected, "{text}");
            assert_eq!(actual.concat(), text);
        }
        // SentenceX can retain a soft line break inside a sentence. Our name
        // protection must nevertheless never join across a line boundary.
        for text in ["J.\nTest", "J.\r\nTest", "J.\u{2029}Test"] {
            assert!(super::super::initialism_byte_ranges(text).is_empty());
            assert!(super::abbreviation_ranges(text).is_empty());
        }
    }
}
