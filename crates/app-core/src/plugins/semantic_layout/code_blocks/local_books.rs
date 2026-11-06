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
            s.section.blocks
                .iter()
                .any(|b| anchor(b).is_some_and(|r| r.start.node == "n40"))
        })
        .unwrap();
    let input = window_input(
        &scope.section,
        &RecognitionRoles::default(),
        0..scope.section.blocks.len(),
        0..scope.section.blocks.len(),
        &[],
        Some(&scope.context),
    );
    assert!(scope.context.iter().any(|t| t.contains("list is nested inside another")));
    assert!(input["blocks"].as_array().unwrap().iter().any(|b|
        b["text"].as_str().is_some_and(|t| t.contains("Nested list structure in HTML"))));
    let scope = &scope.section;
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
        .find(|b| matches!(b,Block::Figure(f) if f.images.iter().any(|i| i.href.path().ends_with("Image00001.gif"))))
        .unwrap();
    let Block::Figure(group) = figure else {
        panic!()
    };
    assert_eq!(group.images.len(), 1);
    let image = &group.images[0];
    assert_eq!(group.captions.len(), 1);
    assert_eq!(group.captions[0].kind, TextBlockKind::Caption);
    assert_eq!(
        group.captions[0].source.as_ref().unwrap().start.node,
        "n265"
    );
    assert_eq!(
        text_block_text(&group.captions[0]),
        "Figure 1.1: Animation of insertion sort in action (time flows down)"
    );
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
        assert!(
            !layout
                .pages
                .iter()
                .flat_map(|p| &p.items)
                .any(|i| matches!(i, PageItem::Quote(_)))
        );
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
    let mut captions = 0;
    for index in 0..source.book().sections.len() {
        let chapter = source.parse_section(index).unwrap();
        for block in &chapter.blocks {
            if let Block::Figure(figure) = block {
                captions += figure
                    .captions
                    .iter()
                    .filter(|t| text_block_text(t).starts_with("Figure "))
                    .count();
            }
        }
    }
    assert_eq!(captions, 116);
    eprintln!(
        "#70 image and caption grouped without AI; 192x216 in book/unified modes; 116 numbered captions associated; #69 C code retains n267"
    );
}
