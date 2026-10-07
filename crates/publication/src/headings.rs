//! Conservative ordinal vocabulary shared by source recovery and presentation.

/// Recognizes a complete division label, never arbitrary leading digits in prose.
/// Appendix letters require the explicit prefix so index letters remain ordinary text.
#[must_use]
pub fn heading_ordinal_key(text: &str) -> Option<String> {
    let normalized = text.trim().to_ascii_lowercase();
    if normalized.len() > 96 {
        return None;
    }
    let trim = |ch: char| ch.is_whitespace() || matches!(ch, ':' | '.' | '-' | '–' | '—');
    let mut rest = normalized.trim_matches(trim);
    if let Some(value) = rest
        .strip_prefix("appendix")
        .or_else(|| rest.strip_prefix("附录"))
    {
        let value = value.trim_matches(trim);
        return (value.len() == 1 && value.bytes().all(|ch| ch.is_ascii_alphabetic()))
            .then(|| format!("appendix:{value}"));
    }
    for prefix in ["chapter", "part", "book", "section"] {
        if let Some(value) = rest.strip_prefix(prefix)
            && value.starts_with(char::is_whitespace)
        {
            rest = value.trim_matches(trim);
            break;
        }
    }
    if let Some(value) = rest.strip_prefix('第') {
        rest = ["部分", "章", "部", "篇", "节", "卷", "册"]
            .iter()
            .find_map(|suffix| value.strip_suffix(suffix))
            .unwrap_or(value)
            .trim();
    }
    if rest.is_empty() {
        return None;
    }
    if rest
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|ch| ch.is_ascii_digit()))
    {
        return Some(rest.to_owned());
    }
    if rest
        .chars()
        .all(|ch| "一二三四五六七八九十百千〇零两".contains(ch))
    {
        return Some(rest.to_owned());
    }
    if rest.len() <= 12 && rest.chars().all(|ch| "ivxlcdm".contains(ch)) {
        return Some(rest.to_owned());
    }
    rest.split([' ', '-'])
        .all(|word| {
            matches!(
                word,
                "one"
                    | "two"
                    | "three"
                    | "four"
                    | "five"
                    | "six"
                    | "seven"
                    | "eight"
                    | "nine"
                    | "ten"
                    | "eleven"
                    | "twelve"
                    | "thirteen"
                    | "fourteen"
                    | "fifteen"
                    | "sixteen"
                    | "seventeen"
                    | "eighteen"
                    | "nineteen"
                    | "twenty"
                    | "thirty"
                    | "forty"
                    | "fifty"
                    | "sixty"
                    | "seventy"
                    | "eighty"
                    | "ninety"
                    | "hundred"
            )
        })
        .then(|| rest.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_complete_division_labels() {
        for value in [
            "2",
            "1.1",
            "CHAPTER 2",
            "Part II",
            "— ONE —",
            "Appendix A",
            "第1章",
            "第二章",
            "第一部分",
            "附录 A",
        ] {
            assert!(heading_ordinal_key(value).is_some(), "{value}");
        }
        for value in [
            "",
            "A",
            "s",
            "Chapter title",
            "2 models",
            "Chapter 2 Introduction",
            "第一章 正文",
            "1..1",
        ] {
            assert!(heading_ordinal_key(value).is_none(), "{value}");
        }
        assert_ne!(heading_ordinal_key("1.1"), heading_ordinal_key("11"));
    }
}
