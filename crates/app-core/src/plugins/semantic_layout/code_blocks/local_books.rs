//! Opt-in read-only regressions. Never configure credentials or issue AI calls.
use super::*;
#[test]
#[ignore = "requires TORTO_ISSUE65_BOOK pointing to the user's local EPUB"]
fn local_structured_writing_nested_html_keeps_its_figure_and_language() {
    let path = std::env::var_os("TORTO_ISSUE65_BOOK").expect("set TORTO_ISSUE65_BOOK");
    let opened = rebook_formats::open_file(std::path::PathBuf::from(path)).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.id.as_str() == "d0e770")
        .unwrap();
    let mut section = source.parse_section(index).unwrap();
    let original: Vec<_> = section
        .blocks
        .iter()
        .flat_map(Block::text_blocks)
        .map(|t| (t.source.clone(), text_block_text(t)))
        .collect();
    let scopes = nested_sections(&section, 0..section.blocks.len());
    let scope = scopes
        .iter()
        .find(|s| {
            s.blocks
                .iter()
                .any(|b| anchor(b).is_some_and(|r| r.start.node == "n40"))
        })
        .unwrap();
    let id = scope
        .blocks
        .iter()
        .position(|b| anchor(b).is_some_and(|r| r.start.node == "n40"))
        .unwrap();
    let p = Proposal::CodeOrQuote {
        body: vec![id],
        presentation: Presentation::Code,
        language: Some("html".into()),
        captions: vec![],
        sources: vec![],
        body_index: None,
    };
    validate_window(
        &[p.clone()],
        scope,
        &RecognitionRoles::default(),
        0..scope.blocks.len(),
        0..scope.blocks.len(),
    )
    .unwrap();
    let a = annotation(&p, scope);
    assert!(cached_valid(&section, &a));
    super::super::compose(&mut section.blocks, &a);
    let after: Vec<_> = section
        .blocks
        .iter()
        .flat_map(Block::text_blocks)
        .map(|t| (t.source.clone(), text_block_text(t)))
        .collect();
    assert_eq!(original, after);
    let code = section
        .blocks
        .iter()
        .flat_map(Block::text_blocks)
        .find(|t| t.source.as_ref().is_some_and(|r| r.start.node == "n40"))
        .unwrap();
    assert_eq!(code.kind, TextBlockKind::Preformatted);
    assert_eq!(code.style.code_language.unwrap().as_str(), "HTML");
    assert!(
        code.content
            .iter()
            .any(|i| matches!(i,Inline::Text(r) if r.style.syntax_colors.is_some()))
    );
    eprintln!(
        "#65 local HTML example: {} nested scopes, source n40 retained and highlighted",
        scopes.len()
    );
}

#[test]
#[ignore = "requires TORTO_ISSUE70_BOOK pointing to the user's local EPUB"]
fn local_algorithm_manual_image_geometry_and_transcribed_c_keep_original_anchors() {
    use rebook_layout::{LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting};
    let path = std::env::var_os("TORTO_ISSUE70_BOOK").expect("set TORTO_ISSUE70_BOOK");
    let opened = rebook_formats::open_file(std::path::PathBuf::from(path)).unwrap();
    let source = opened.source();
    let mut section = source.parse_section(0).unwrap();
    let figure = section
        .blocks
        .iter()
        .find(|b| matches!(b,Block::Image(i) if i.href.path().ends_with("Image00001.gif")))
        .unwrap();
    let Block::Image(image) = figure else {
        panic!()
    };
    assert!(image.style.container_width.is_none());
    assert_eq!(image.source.as_ref().unwrap().start.node, "n263");
    for unified in [false, true] {
        let mut style = ReaderStyle::default();
        if unified {
            style.typesetting = ReaderTypesetting::unified();
        }
        let layout = LayoutEngine::new()
            .layout_blocks(
                source.as_ref(),
                std::slice::from_ref(figure),
                LayoutViewport::new(800, 700).unwrap(),
                &style,
            )
            .unwrap();
        let placed = layout
            .pages
            .iter()
            .flat_map(|p| &p.items)
            .find_map(|i| {
                if let PageItem::Image(i) = i {
                    Some(i)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!((placed.width, placed.height), (192.0, 216.0));
    }
    let code_index = section
        .blocks
        .iter()
        .position(|b| matches!(b,Block::Image(i) if i.href.path().ends_with("Image00002.gif")))
        .unwrap();
    let Block::Image(code) = &section.blocks[code_index] else {
        panic!()
    };
    let href = code.href.clone();
    super::super::formulas::compose_text(
        &mut section.blocks,
        &href,
        &rebook_publication::ImageText {
            text: "insertion_sort(item s[], int n)\n{\n    int i,j;\n}".into(),
            literal: true,
            translated: None,
        },
    );
    assert!(eligible_ids(&section).contains(&code_index));
    let p = Proposal::CodeOrQuote {
        body: vec![code_index],
        presentation: Presentation::Code,
        language: Some("c".into()),
        captions: vec![],
        sources: vec![],
        body_index: None,
    };
    let a = annotation(&p, &section);
    super::super::compose(&mut section.blocks, &a);
    let code = section
        .blocks
        .iter()
        .flat_map(Block::text_blocks)
        .find(|t| t.source.as_ref().is_some_and(|r| r.start.node == "n267"))
        .unwrap();
    assert_eq!(code.kind, TextBlockKind::Preformatted);
    assert_eq!(code.style.code_language.unwrap().as_str(), "C");
    eprintln!("#70 image restored to 192x216 in book/unified modes; #69 C code retains n267");
}
