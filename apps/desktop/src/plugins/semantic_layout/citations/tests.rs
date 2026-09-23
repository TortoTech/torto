use super::super::tests::{section, text};
use super::*;

#[test]
fn citations_keep_original_text_and_number_across_style_runs() {
    let Block::Text(mut t) = text("p", "Some evidence (Smith, 2020) and more (Jones 23–25).")
    else {
        panic!()
    };
    let original = text_block_text(&t);
    let found = candidates(&t);
    assert_eq!(found.len(), 2);
    let Inline::Text(r) = t.content[0].clone() else {
        panic!()
    };
    let split = original.find("2020").unwrap();
    let mut first = r.clone();
    first.text = original[..split].into();
    let mut second = r;
    second.text = original[split..].into();
    second.style.italic = true;
    t.content = vec![Inline::Text(first), Inline::Text(second)];
    apply(&mut t, &found);
    assert_eq!(text_block_text(&t), original);
    assert!(
        t.content
            .iter()
            .any(|i| matches!(i,Inline::Text(r) if r.style.inline_citation==1 && r.style.italic))
    );
    assert!(
        t.content
            .iter()
            .any(|i| matches!(i,Inline::Text(r) if r.style.inline_citation==2))
    );
}

#[test]
fn candidates_protect_footnotes_and_narrative_years() {
    let Block::Text(mut t) = text(
        "p",
        "Smith (2020) argues this [12] (ordinary aside) (张三，2020).",
    ) else {
        panic!()
    };
    let c = candidates(&t);
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].text, "[12]");
    let Inline::Text(r) = &mut t.content[0] else {
        panic!()
    };
    r.style.inline_role = InlineRole::Footnote;
    assert!(candidates(&t).is_empty());
}

#[test]
fn cached_citations_validate_their_source_and_ranges() {
    let b = text("p", "Evidence (Smith, 2020).");
    let Block::Text(t) = &b else { panic!() };
    let a = Annotation::InlineCitations {
        source: t.source.clone().unwrap(),
        spans: candidates(t),
    };
    let s = section(vec![b]);
    let cached: Annotation = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    assert!(validate_annotations(&s, std::slice::from_ref(&cached)));
    let source = super::super::tests::original_source(s.clone());
    let overlay = SemanticLayoutSource::new(source.clone(), source.clone());
    assert!(overlay.install(
        0,
        Recognition {
            formulas_checked: true,
            fingerprint: fingerprint(&s),
            annotations: vec![cached.clone()],
            skipped_groups: 0
        }
    ));
    let displayed = overlay.parse_section(0).unwrap();
    assert_ne!(displayed.blocks, s.blocks);
    overlay.clear();
    assert_eq!(overlay.parse_section(0).unwrap().blocks, s.blocks);
    let mut changed = s.clone();
    changed.blocks[0] = text("p", "Other text (Jones, 1990).");
    assert!(!validate_annotations(&changed, &[cached]));
}

#[test]
#[ignore = "requires TORTO_CITATION_BOOK; uses the configured gemini/lite model on the photographed paragraph"]
fn live_computational_models_inline_citations() {
    let opened = rebook_formats::open_file(std::env::var("TORTO_CITATION_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().ends_with("09_chapter2.xhtml"))
        .unwrap();
    let mut s = source.parse_section(index).unwrap();
    let target=s.blocks.iter().find(|b| matches!(b,Block::Text(t) if text_block_text(t).contains("Lewandowsky, 1991") && text_block_text(t).contains("McClosky"))).unwrap().clone();
    s.blocks = vec![target];
    let settings = PluginSettings::load_default().unwrap();
    let provider = settings
        .providers
        .iter()
        .find(|p| p.models.iter().any(|m| m.id == "gemini/lite"))
        .unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(recognize(
            &reqwest::Client::new(),
            provider,
            "gemini/lite",
            &s,
        ))
        .unwrap();
    assert!(validate_annotations(&s, &result));
    assert_eq!(result.len(), 1);
    let Annotation::InlineCitations {
        source: range,
        spans,
    } = &result[0]
    else {
        panic!()
    };
    for c in spans {
        println!("citation: {}", c.text);
    }
    assert_eq!(spans.len(), 4);
    for (c, name) in spans
        .iter()
        .zip(["Rumelhart", "Crick", "Hebb", "Lewandowsky"])
    {
        assert!(c.text.contains(name));
    }
    let mut blocks = s.blocks.clone();
    compose(&mut blocks, range, spans);
    let Block::Text(before) = &s.blocks[0] else {
        panic!()
    };
    let Block::Text(after) = &blocks[0] else {
        panic!()
    };
    assert_eq!(text_block_text(before), text_block_text(after));
    assert_eq!(after.source, before.source);
    let numbered: Vec<_> = after
        .content
        .iter()
        .filter_map(|i| match i {
            Inline::Text(r) if r.style.inline_citation > 0 => Some(r.style.inline_citation),
            _ => None,
        })
        .collect();
    for n in 1..=4 {
        assert!(numbered.contains(&n));
    }
    println!(
        "Verified four citation groups; ordinary asides and original paragraph text preserved"
    );
}
