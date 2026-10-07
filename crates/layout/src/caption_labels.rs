//! Presentation-only normalization of numbered figure and table captions.
use rebook_publication::{Inline, TextRun};

/// A table caption containing its identifier and optional punctuation only.
pub(super) fn table_label_only(text: &str) -> bool {
    let text = text.trim();
    let number = ["table", "tab.", "表格", "表"]
        .into_iter()
        .find_map(|name| {
            text.get(..name.len())
                .filter(|prefix| prefix.eq_ignore_ascii_case(name))
                .map(|_| {
                    text[name.len()..]
                        .trim_start()
                        .trim_start_matches(['.', ':', '：', '．'])
                        .trim_start()
                })
        })
        .unwrap_or(text);
    let canonical = format!("Table {number}");
    label(&canonical).is_some_and(|(_, end)| {
        canonical[end..]
            .chars()
            .all(|c| c.is_whitespace() || ".:：．。–—-".contains(c))
    })
}

pub(super) fn normalize(content: &mut Vec<Inline>) {
    // Stop at media: only an authored textual prefix can be a caption label.
    let leading = content
        .iter()
        .take_while(|inline| matches!(inline, Inline::Text(_)))
        .filter_map(|inline| match inline {
            Inline::Text(run) => Some(run.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let Some((start, end)) = label(&leading) else {
        return;
    };
    let mut gap_start = end;
    let rest = &leading[end..];
    let trimmed = rest.trim_start_matches(char::is_whitespace);
    if let Some(punctuation) = trimmed.chars().next().filter(|c| {
        matches!(
            c,
            '.' | ':' | '\u{ff1a}' | '\u{ff0e}' | '\u{3002}' | '\u{2014}' | '\u{2013}'
        )
    }) {
        gap_start = leading.len() - trimmed.len() + punctuation.len_utf8();
    }
    let gap_end = leading.len()
        - leading[gap_start..]
            .trim_start_matches(char::is_whitespace)
            .len();
    let body = gap_end < leading.len()
        || content
            .iter()
            .skip_while(|inline| matches!(inline, Inline::Text(_)))
            .any(|inline| !matches!(inline, Inline::Break));
    let gap = body.then_some(gap_start..gap_end);
    let original = std::mem::take(content);
    let mut offset = 0;
    let mut inserted = false;
    let mut last_style = None;
    for inline in original {
        let Inline::Text(run) = inline else {
            if !inserted && gap.as_ref().is_some_and(|gap| gap.start == offset) {
                if let Some(style) = last_style {
                    content.push(Inline::Text(TextRun {
                        text: " ".into(),
                        style,
                        link: None,
                    }));
                }
                inserted = true;
            }
            content.push(inline);
            // The prefix never spans a media or explicit break boundary.
            offset = leading.len() + 1;
            continue;
        };
        last_style = Some(run.style);
        let mut cuts = vec![0, run.text.len()];
        for boundary in [start, end]
            .into_iter()
            .chain(gap.iter().flat_map(|gap| [gap.start, gap.end]))
        {
            if boundary >= offset && boundary <= offset + run.text.len() {
                cuts.push(boundary - offset);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for bytes in cuts.windows(2) {
            let at = offset + bytes[0];
            if !inserted && gap.as_ref().is_some_and(|gap| gap.start == at) {
                content.push(Inline::Text(TextRun {
                    text: " ".into(),
                    style: run.style,
                    link: None,
                }));
                inserted = true;
            }
            if gap
                .as_ref()
                .is_some_and(|gap| at >= gap.start && at < gap.end)
            {
                continue;
            }
            let mut part = run.clone();
            part.text = run.text[bytes[0]..bytes[1]].into();
            if at >= start && at < end {
                part.style.bold = true;
            }
            content.push(Inline::Text(part));
        }
        offset += run.text.len();
    }
}

fn label(text: &str) -> Option<(usize, usize)> {
    let trimmed = text.trim_start_matches(char::is_whitespace);
    let start = text.len() - trimmed.len();
    let name = [
        "\u{56fe}\u{7247}",
        "\u{56fe}\u{8868}",
        "\u{63d2}\u{56fe}",
        "\u{56fe}\u{7248}",
        "\u{8868}\u{683c}",
        "\u{56fe}",
        "\u{8868}",
        "illustration",
        "figure",
        "table",
        "plate",
        "chart",
        "exhibit",
        "fig.",
        "fig",
        "tab.",
    ]
    .into_iter()
    .find(|name| {
        trimmed
            .get(..name.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
    })?;
    let after_name = start + name.len();
    let rest = text[after_name..].trim_start_matches(char::is_whitespace);
    let number_start = text.len() - rest.len();
    let mut chars = rest.char_indices().peekable();
    let first = chars.peek()?.1;
    // Appendix/supplement numbers such as A.1 and S2; don't consume body words.
    if first.is_ascii_uppercase()
        && rest
            .chars()
            .nth(1)
            .is_some_and(|c| digit(c) || c == '.' || c == '-')
    {
        chars.next();
        if chars.peek().is_some_and(|(_, c)| *c == '.' || *c == '-') {
            chars.next();
        }
    }
    let chinese = "一二三四五六七八九十百千零〇".contains(first);
    let mut end = number_start;
    while let Some((index, c)) = chars.peek().copied() {
        if digit(c) || chinese && "\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}\u{516d}\u{4e03}\u{516b}\u{4e5d}\u{5341}\u{767e}\u{5343}\u{96f6}\u{3007}".contains(c) {
            end = number_start + index + c.len_utf8(); chars.next();
        } else if matches!(c, '.' | '-' | '\u{ff0e}' | '\u{2013}') && chars.clone().nth(1).is_some_and(|(_, next)| digit(next)) {
            chars.next();
        } else { break; }
    }
    if end == number_start {
        // Roman numerals require a word boundary to avoid eating an English title.
        if number_start == after_name {
            return None;
        }
        let roman = rest
            .chars()
            .take_while(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M'))
            .count();
        if roman == 0
            || rest[roman..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic())
        {
            return None;
        }
        end = number_start + roman;
    }
    if text[end..].starts_with('(') || text[end..].starts_with('\u{ff08}') {
        let suffix = &text[end..];
        let mut suffix_chars = suffix.char_indices();
        suffix_chars.next();
        let (_, part) = suffix_chars.next()?;
        if part.is_ascii_alphanumeric()
            && let Some((close_index, close)) = suffix_chars.next()
            && matches!(close, ')' | '\u{ff09}')
        {
            end += close_index + close.len_utf8();
        }
    }
    Some((start, end))
}

fn digit(c: char) -> bool {
    c.is_ascii_digit() || ('\u{ff10}'..='\u{ff19}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReaderStyle, ReaderTypesetting, TextContext, resolve_text_block};
    use rebook_publication::{MathRun, PublicationUrl, TextBlock, TextBlockKind, TextStyle};

    fn run(text: &str) -> Inline {
        Inline::Text(TextRun {
            text: text.into(),
            style: TextStyle::default(),
            link: None,
        })
    }
    fn text(content: &[Inline]) -> String {
        content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(run) => Some(run.text.as_str()),
                _ => None,
            })
            .collect()
    }
    fn bold(content: &[Inline]) -> String {
        content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(run) if run.style.bold => Some(run.text.as_str()),
                _ => None,
            })
            .collect()
    }
    #[test]
    fn restores_chinese_and_english_caption_spacing_and_only_bolds_labels() {
        for (input, expected, prefix) in [
            (
                "\u{56fe}1.2\u{8bf4}\u{660e}",
                "\u{56fe}1.2 \u{8bf4}\u{660e}",
                "\u{56fe}1.2",
            ),
            (
                "\u{8868} 1.2.3\u{ff1a}\u{5185}\u{5bb9}",
                "\u{8868} 1.2.3\u{ff1a} \u{5185}\u{5bb9}",
                "\u{8868} 1.2.3",
            ),
            (
                "Figure 1.2Description",
                "Figure 1.2 Description",
                "Figure 1.2",
            ),
            (
                "FIGURE 1.2.   Description",
                "FIGURE 1.2. Description",
                "FIGURE 1.2",
            ),
            ("Fig. 2(a)Description", "Fig. 2(a) Description", "Fig. 2(a)"),
            (
                "Table A.1:Description",
                "Table A.1: Description",
                "Table A.1",
            ),
            (
                "Table S2\u{a0}\u{a0}Description",
                "Table S2 Description",
                "Table S2",
            ),
            ("Table IV Description", "Table IV Description", "Table IV"),
            (
                "\u{56fe}1\u{4e00}\u{53ea}\u{9e1f}",
                "\u{56fe}1 \u{4e00}\u{53ea}\u{9e1f}",
                "\u{56fe}1",
            ),
            (
                "\u{8868}\u{4e8c}\u{ff1a}\u{5185}\u{5bb9}",
                "\u{8868}\u{4e8c}\u{ff1a} \u{5185}\u{5bb9}",
                "\u{8868}\u{4e8c}",
            ),
            (
                "\u{56fe}\u{ff11}\u{ff0e}\u{ff12}\u{8bf4}\u{660e}",
                "\u{56fe}\u{ff11}\u{ff0e}\u{ff12} \u{8bf4}\u{660e}",
                "\u{56fe}\u{ff11}\u{ff0e}\u{ff12}",
            ),
        ] {
            let mut content = vec![run(input)];
            normalize(&mut content);
            assert_eq!(text(&content), expected, "{input}");
            assert_eq!(bold(&content).trim_end(), prefix, "{input}");
            normalize(&mut content);
            assert_eq!(
                text(&content),
                expected,
                "normalizing twice cannot duplicate the gap"
            );
        }
    }
    #[test]
    fn preserves_links_emphasis_notes_and_math_across_split_prefix_runs() {
        let mut linked = TextRun {
            text: "Description".into(),
            style: TextStyle {
                italic: true,
                ..Default::default()
            },
            link: Some(PublicationUrl::website("https://example.com").unwrap()),
        };
        let note = TextRun {
            text: "1".into(),
            style: TextStyle {
                link_role: rebook_publication::LinkRole::FootnoteReference,
                ..Default::default()
            },
            link: None,
        };
        let formula = Inline::Math(MathRun {
            latex: "x".into(),
            display: false,
            size_scale: 1.0,
            original: None,
        });
        let mut content = vec![
            run("Fig"),
            run(". "),
            run("2."),
            run("3"),
            Inline::Text(linked.clone()),
            Inline::Text(note.clone()),
            formula.clone(),
        ];
        normalize(&mut content);
        assert_eq!(text(&content), "Fig. 2.3 Description1");
        assert!(content.contains(&Inline::Text(linked.clone())));
        assert!(content.contains(&Inline::Text(note)));
        assert!(content.contains(&formula));
        linked.style.bold = true;
        let mut already_bold = vec![run("Table 1"), Inline::Text(linked.clone())];
        normalize(&mut already_bold);
        assert!(already_bold.contains(&Inline::Text(linked)));
        let mut math_first = vec![run("Figure 2"), formula.clone(), run(" explanation")];
        normalize(&mut math_first);
        assert_eq!(text(&math_first), "Figure 2  explanation");
        assert_eq!(math_first[1], run(" "));
        assert!(math_first.contains(&formula));
    }
    #[test]
    fn applies_only_to_unified_caption_presentation() {
        let caption = TextBlock {
            kind: TextBlockKind::Caption,
            content: vec![run("Table 4.1Description")],
            style: Default::default(),
            source: None,
        };
        let original = caption.clone();
        let unified = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..Default::default()
        };
        let resolved = resolve_text_block(&caption, &unified, TextContext::Flow);
        assert_eq!(text(&resolved.content), "Table 4.1 Description");
        assert_eq!(bold(&resolved.content), "Table 4.1");
        let classic = resolve_text_block(&caption, &ReaderStyle::default(), TextContext::Flow);
        assert_eq!(text(&classic.content), "Table 4.1Description");
        assert!(bold(&classic.content).is_empty());
        let prose = TextBlock {
            kind: TextBlockKind::Paragraph,
            ..caption.clone()
        };
        assert_eq!(
            text(&resolve_text_block(&prose, &unified, TextContext::Flow).content),
            "Table 4.1Description"
        );
        assert_eq!(caption, original);
        for value in [
            "Table of contents",
            "Figurehead description",
            "An explanation of Figure 1.2",
            "Table Results",
            "Figure",
        ] {
            let mut content = vec![run(value)];
            normalize(&mut content);
            assert_eq!(text(&content), value);
            assert!(bold(&content).is_empty());
        }
    }
}
