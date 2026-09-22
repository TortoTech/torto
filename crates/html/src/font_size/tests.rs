use crate::*;

fn paragraph_runs(xml: &str) -> Vec<TextRun> {
    let descriptor = SpineItem {
        id: SpineItemId::new("sizes").unwrap(),
        href: PublicationUrl::parse("sizes.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: Vec::new(),
    };
    let section = parse_section(xml, &descriptor, |_| None).unwrap();
    section
        .blocks
        .into_iter()
        .flat_map(|block| {
            let Block::Text(text) = block else {
                panic!("expected paragraph")
            };
            text.content.into_iter().filter_map(|inline| match inline {
                Inline::Text(run) => Some(run),
                _ => None,
            })
        })
        .collect()
}

fn assert_sizes(xml: &str, expected: &[f32]) {
    let runs = paragraph_runs(xml);
    assert_eq!(runs.len(), expected.len(), "{runs:?}");
    for (run, expected) in runs.iter().zip(expected) {
        assert!(
            (run.style.size_scale - expected).abs() < 0.0001,
            "{}: expected {expected}, got {}",
            run.text,
            run.style.size_scale
        );
    }
}

#[test]
fn absolute_keywords_do_not_compound_with_ancestors() {
    for (keyword, expected) in [
        ("xx-small", 0.6),
        ("x-small", 0.75),
        ("small", 8.0 / 9.0),
        ("medium", 1.0),
        ("large", 1.2),
        ("x-large", 1.5),
        ("xx-large", 2.0),
        ("xxx-large", 3.0),
    ] {
        assert_sizes(
            &format!(
                r#"<html style="font-size:150%"><body style="font-size:2em"><p style="font-size:{keyword}">outer<br/><span style="font-size:{keyword}">inner</span></p></body></html>"#
            ),
            &[expected, expected],
        );
    }
}

#[test]
fn relative_units_use_parent_but_rem_uses_computed_root() {
    assert_sizes(
        r#"<html style="font-size:2rem"><body style="font-size:150%"><p>parent<br/><span style="font-size:50%">percent</span><br/><span style="font-size:0.5em">em</span><br/><span style="font-size:0.5rem">rem</span><br/><span style="font-size:smaller">smaller<br/><span style="font-size:larger">larger</span></span></p></body></html>"#,
        &[3.0, 1.5, 1.5, 1.0, 2.5, 3.0],
    );
}

#[test]
fn absolute_lengths_and_initial_do_not_use_parent_or_root() {
    assert_sizes(
        r#"<html style="font-size:200%"><body style="font-size:2em"><p><br/><span style="font-size:12px">px<br/><span style="font-size:12px">nested</span></span><br/><span style="font-size:9pt">pt</span><br/><span style="font-size:initial">initial</span><br/><span style="font-size:inherit">inherit</span><br/><span style="font-size:unset">unset</span></p></body></html>"#,
        &[0.75, 0.75, 0.75, 1.0, 4.0, 4.0],
    );
}

#[test]
fn root_css_is_resolved_after_stylesheets_and_inline_cascade() {
    assert_sizes(
        r#"<html style="font-size:150%"><head><style>html { font-size: 200%; } body { font-size: 2em; } .small { font-size: X-SMALL; } .root { font-size: 1rem; }</style></head><body><p>E<br/><span class="small">ACH TIME HE SPINS IT</span><br/><span class="root">root</span></p></body></html>"#,
        &[3.0, 0.75, 1.5],
    );
}

#[test]
fn invalid_sizes_preserve_earlier_declarations_or_inheritance() {
    for value in ["nonsense", "-1em", "NaNpx", "infem", "1e99%", "12", "1 em"] {
        assert_sizes(
            &format!(
                r#"<html><head><style>.s {{ font-size: small; }}</style></head><body style="font-size:2em"><p><br/><span class="s" style="font-size:{value}">cascade</span><br/><span style="font-size:{value}">inherit</span><br/><span style="font-size:12px;font-size:{value}">duplicate</span></p></body></html>"#
            ),
            &[8.0 / 9.0, 2.0, 0.75],
        );
    }
}

#[test]
fn css_inheritance_overrides_semantic_heading_and_small_defaults() {
    assert_sizes(
        r#"<html><body style="font-size:2em"><h1 style="font-size:inherit">heading</h1><p><small style="font-size:unset">small</small><br/><span style="font-size:0">zero</span></p></body></html>"#,
        &[2.0, 2.0, 0.0],
    );
}

#[test]
fn keyword_sizes_survive_inheritance_and_absolute_keywords_do_not_compound() {
    for (keyword, expected) in [
        ("xx-small", 0.6),
        ("x-small", 0.75),
        ("small", 8.0 / 9.0),
        ("medium", 1.0),
        ("large", 1.2),
        ("x-large", 1.5),
        ("xx-large", 2.0),
        ("xxx-large", 3.0),
    ] {
        let runs = paragraph_runs(&format!(
            r#"<html><body><p style="font-size:{keyword}">outer<br/><span>inherited</span><br/><span style="font-size:{keyword}">nested</span></p></body></html>"#
        ));
        for run in runs {
            assert!((run.style.keyword_size_scale.unwrap() - expected).abs() < 0.0001);
        }
    }
    let runs = paragraph_runs(
        r#"<html><body><p style="font-size:x-large"><span style="font-size:smaller">small<br/><span style="font-size:larger">large</span></span><br/><span style="font-size:inherit">inherit</span><br/><span style="font-size:unset">unset</span><br/><span style="font-size:12px">pixels</span><br/><span style="font-size:0.5em">em</span><br/><span style="font-size:initial">reset</span><br/><span style="font-size:garbage">invalid</span></p></body></html>"#,
    );
    let expected = [
        Some(1.25),
        Some(1.5),
        Some(1.5),
        Some(1.5),
        None,
        None,
        None,
        Some(1.5),
    ];
    assert_eq!(runs.len(), expected.len());
    for (run, expected) in runs.iter().zip(expected) {
        assert_eq!(run.style.keyword_size_scale, expected, "{}", run.text);
    }
}

#[test]
fn body_keywords_do_not_replace_implicit_heading_sizes() {
    let runs = paragraph_runs(
        r#"<html><body style="font-size:large"><h1>heading</h1><h2 style="font-size:small">small heading</h2><h3 style="font-size:inherit">inherited heading</h3><p>body</p></body></html>"#,
    );
    assert_eq!(
        runs.iter()
            .map(|run| run.style.keyword_size_scale)
            .collect::<Vec<_>>(),
        vec![None, Some(8.0 / 9.0), Some(1.2), Some(1.2)]
    );
}

/// Extracted EPUB resources, with XHTML sanitized like the EPUB import path
/// (external DOCTYPE removed, named HTML entities expanded). Each book directory
/// lists resource-relative XHTML paths in chapters.txt. No book content is stored
/// in the repository.
#[test]
#[ignore = "requires TORTO_FONT_SIZE_FIXTURES with extracted book resources"]
fn local_books_resolve_x_small_styles() {
    let root = std::path::PathBuf::from(std::env::var("TORTO_FONT_SIZE_FIXTURES").unwrap());
    let mut total_matched = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let book = entry.unwrap().path();
        let mut matched = 0;
        for path in std::fs::read_to_string(book.join("chapters.txt"))
            .unwrap()
            .lines()
        {
            let xml = std::fs::read_to_string(book.join(path)).unwrap();
            let document = Document::parse(&xml).unwrap();
            let href = PublicationUrl::parse(path).unwrap();
            let sheet = StyleSheet::from_document(&document, &href, &mut |url| {
                std::fs::read_to_string(book.join(url.path())).ok()
            });
            for node in document.descendants().filter(Node::is_element) {
                if sheet
                    .cascaded_properties(node)
                    .get("font-size")
                    .map(String::as_str)
                    == Some("x-small")
                {
                    let style = sheet.text_style_for_block(node, TextBlockKind::Paragraph);
                    assert!((style.size_scale - 0.75).abs() < 0.0001, "{path}");
                    matched += 1;
                }
            }
            let descriptor = SpineItem {
                id: SpineItemId::new("local").unwrap(),
                href,
                media_type: "application/xhtml+xml".into(),
                linear: true,
                properties: Vec::new(),
            };
            let section = parse_section(&xml, &descriptor, |url| {
                std::fs::read_to_string(book.join(url.path())).ok()
            })
            .unwrap();
            assert!(!section.blocks.is_empty());
        }
        // A bundled stylesheet can declare x-small without any matching content.
        total_matched += matched;
        println!(
            "{}: {matched} x-small elements resolved",
            book.file_name().unwrap().to_string_lossy()
        );
    }
    assert!(
        total_matched > 0,
        "fixtures must include active x-small declarations"
    );
}
