use super::super::{
    Inline, TextBlock, TextBlockKind, TextRun, apply_sentence_structure, inline_text,
    paragraph_atoms_for_content,
};
use rebook_publication::{
    LinkRole, MathRun, PublicationUrl, SourceAnchor, SourceRange, SpineItemId, TextBaseline,
};

fn text(value: &str) -> Inline {
    Inline::Text(TextRun {
        text: value.into(),
        style: Default::default(),
        link: None,
    })
}

fn assert_atoms(value: &str, expected: &[&str]) {
    let atoms = paragraph_atoms_for_content(&[text(value)], "en");
    assert_eq!(
        atoms.iter().map(|a| a.text.trim()).collect::<Vec<_>>(),
        expected,
        "{value}"
    );
    let chars: Vec<_> = value.chars().collect();
    let mut end = 0;
    for atom in atoms {
        assert_eq!(atom.start, end);
        assert_eq!(
            atom.text,
            chars[atom.start..atom.end].iter().collect::<String>()
        );
        end = atom.end;
    }
    assert_eq!(end, chars.len());
}

#[test]
fn single_sentence_continuations_include_library_examples_without_length_limits() {
    for value in [
        "是这个道理：没道理。",
        "难怪我们会经常慨叹：怎么就写成这样了？",
        "我的意思是：翻译就是翻译。",
        "还有一个问题必须解决：学习。",
        "此时此刻，你的脑正在完成一件惊人的壮举：阅读。",
        "接：稍等。",
        "只能幽幽开口：“帅。”",
        "Consider this simple question: What is your birth date?",
        "Note: This explanation is intentionally longer than six words and forty characters.",
        "说明: 这是一个没有中间标点并且明显超过十二个字的完整解释句。",
    ] {
        assert_atoms(value, &[value]);
    }
    let value = format!("说明：{}。", "这是一段很长的连续解释".repeat(128));
    assert_atoms(&value, &[&value]);
}

#[test]
fn intermediate_punctuation_and_explicit_breaks_keep_colon_boundaries() {
    for (value, expected) in [
        ("说明：先理解，再表达。", vec!["说明：", "先理解，再表达。"]),
        ("项目：甲、乙。", vec!["项目：", "甲、乙。"]),
        (
            "说明：先理解；再表达。",
            vec!["说明：", "先理解；", "再表达。"],
        ),
        ("原因：结论：没有理由。", vec!["原因：", "结论：没有理由。"]),
        ("Note: First, continue.", vec!["Note:", "First, continue."]),
        ("说明：\n下一步。", vec!["说明：", "下一步。"]),
        ("说明：未完…之后继续。", vec!["说明：", "未完…之后继续。"]),
        (
            "Note: Wait... Then continue.",
            vec!["Note:", "Wait...", "Then continue."],
        ),
    ] {
        assert_atoms(value, &expected);
    }
}

#[test]
fn complete_single_quoted_sentences_join_but_multi_sentence_passages_stay_separate() {
    for (value, expected) in [
        (
            "他说：“快走！”第二天再见。",
            vec!["他说：“快走！”", "第二天再见。"],
        ),
        (
            "He said: \"Go!\" Then he left.",
            vec!["He said: \"Go!\"", "Then he left."],
        ),
        ("他说：‘快走！’", vec!["他说：‘快走！’"]),
        ("他说：「快走！」", vec!["他说：「快走！」"]),
        ("他说：“她喊‘快走！’”", vec!["他说：“她喊‘快走！’”"]),
        (
            "Note: \"Dr. Smith arrived.\" Next step.",
            vec!["Note: \"Dr. Smith arrived.\"", "Next step."],
        ),
        (
            "他说：“第一句。第二句。”后面继续。",
            vec!["他说：", "“第一句。第二句。”", "后面继续。"],
        ),
        (
            "He said: \"First sentence. Second sentence.\" Then he left.",
            vec![
                "He said:",
                "\"First sentence. Second sentence.\"",
                "Then he left.",
            ],
        ),
        (
            "他说：“没事。我先走了。”",
            vec!["他说：", "“没事。我先走了。”"],
        ),
        ("说明：（只有一句。）", vec!["说明：（只有一句。）"]),
        (
            "说明：（第一句。第二句。）",
            vec!["说明：", "（第一句。第二句。）"],
        ),
        (
            "他说：“先理解，再表达。”",
            vec!["他说：", "“先理解，再表达。”"],
        ),
        ("他说：“还没结束。", vec!["他说：", "“还没结束。"]),
    ] {
        assert_atoms(value, &expected);
    }
}

#[test]
fn ordinary_sentence_ends_and_technical_periods_are_preserved() {
    for (value, expected) in [
        (
            "说明：第一句结束。第二句继续。",
            vec!["说明：第一句结束。", "第二句继续。"],
        ),
        (
            "Note: Dr. Smith arrived. Next step.",
            vec!["Note: Dr. Smith arrived.", "Next step."],
        ),
        (
            "Note: Use 3.14 today. Next step.",
            vec!["Note: Use 3.14 today.", "Next step."],
        ),
        (
            "时间：12:30。比例：1：2。",
            vec!["时间：12:30。", "比例：1：2。"],
        ),
        (
            "网址https://example.com:8080/a，继续。",
            vec!["网址https://example.com:8080/a，继续。"],
        ),
        (
            "“说明：保持完整”以及（备注：不拆开）。",
            vec!["“说明：保持完整”以及（备注：不拆开）。"],
        ),
    ] {
        assert_atoms(value, &expected);
    }
}

#[test]
fn inline_styles_formulas_links_references_and_source_ranges_survive_composition() {
    let spine = SpineItemId::new("chapter").unwrap();
    let mut source = SourceRange {
        start: SourceAnchor {
            spine: spine.clone(),
            node: "n1".into(),
            text_offset: 17,
        },
        end: SourceAnchor {
            spine,
            node: "n1".into(),
            text_offset: 47,
        },
    };
    let mut footnote = text("1");
    let Inline::Text(run) = &mut footnote else {
        unreachable!()
    };
    run.link = Some(PublicationUrl::parse("chapter.xhtml#note").unwrap());
    run.style.link_role = LinkRole::FootnoteReference;
    run.style.baseline = TextBaseline::Superscript;
    let formula = Inline::Math(MathRun {
        latex: "x:y;z".into(),
        original: None,
        display: false,
        size_scale: 1.0,
    });
    let mut link = text("https://example.com/a;b:c");
    let Inline::Text(run) = &mut link else {
        unreachable!()
    };
    run.link = PublicationUrl::website("https://example.com/a;b:c");
    assert!(run.link.is_some());
    let mut emphasized = text("没道理");
    let Inline::Text(run) = &mut emphasized else {
        unreachable!()
    };
    run.style.bold = true;
    let original = vec![
        text("是这个道理："),
        emphasized.clone(),
        formula.clone(),
        link.clone(),
        text("。"),
        footnote.clone(),
        text("然后继续。"),
    ];
    source.end.text_offset =
        source.start.text_offset + inline_text(&original).chars().count() as u64;
    let mut block = TextBlock {
        kind: TextBlockKind::Paragraph,
        content: original.clone(),
        style: Default::default(),
        source: Some(source.clone()),
    };
    apply_sentence_structure(&mut block, "zh");
    assert_eq!(
        inline_text(&block.content).replace('\n', ""),
        inline_text(&original)
    );
    assert_eq!(
        block
            .content
            .iter()
            .filter(|i| matches!(i, Inline::Break))
            .count(),
        1
    );
    assert_eq!(block.source, Some(source));
    for inline in [&emphasized, &formula, &link, &footnote] {
        assert_eq!(block.content.iter().filter(|i| *i == inline).count(), 1);
    }
    let end = block
        .content
        .iter()
        .position(|i| matches!(i, Inline::Break))
        .unwrap();
    assert_eq!(block.content[end - 1], footnote);
}
