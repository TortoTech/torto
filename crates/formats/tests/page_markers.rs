//! Local source regression; the publication stays outside the repository.
use rebook_publication::{Block, Inline, TextBlockKind};

#[test]
#[ignore = "requires TORTO_BOOKS_WITHOUT_BARRIERS_EPUB; read-only issue #47 regression"]
fn local_workflow_list_has_no_line_from_its_print_page_wrapper() {
    let path = std::env::var_os("TORTO_BOOKS_WITHOUT_BARRIERS_EPUB").unwrap();
    let opened = rebook_formats::open_file(path).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|section| section.href.path().ends_with("chapter-3.xhtml"))
        .unwrap();
    let section = source.parse_section(index).unwrap();
    let item = section
        .blocks
        .iter()
        .find_map(|block| {
            let Block::Text(text) = block else {
                return None;
            };
            let plain: String = text
                .content
                .iter()
                .flat_map(Inline::text_runs)
                .map(|run| run.text.as_str())
                .collect();
            (matches!(text.kind, TextBlockKind::ListItem { ordinal: 1, .. })
                && plain.starts_with("In a print-first workflow"))
            .then_some(text)
        })
        .unwrap();
    assert!(
        !item
            .content
            .iter()
            .any(|inline| matches!(inline, Inline::Break))
    );
    let marker = section
        .anchors
        .iter()
        .find(|anchor| anchor.fragment == "page-59")
        .unwrap();
    assert_eq!(marker.page_label.as_deref(), Some("48"));
    assert_eq!(&marker.source, &item.source.as_ref().unwrap().end);
}
