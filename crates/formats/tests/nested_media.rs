use rebook_publication::{Block, Inline};

#[test]
#[ignore = "requires TORTO_MEDIA_BOOK local EPUB"]
fn local_tinnitus_nested_figures() {
    let opened = rebook_formats::open_file(std::env::var("TORTO_MEDIA_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let mut figures = 0;
    let mut found = false;
    for index in 0..source.book().sections.len() {
        let section = source.parse_section(index).unwrap();
        for block in section.blocks {
            if let Block::Figure(figure) = block {
                if !figure.captions.is_empty() {
                    figures += 1;
                }
                if figure
                    .images
                    .iter()
                    .any(|image| image.href.path().contains("fig02.06"))
                {
                    found = true;
                    assert_eq!(figure.captions.len(), 1);
                    assert!(
                        figure.captions[0].content.iter().any(
                            |i| matches!(i,Inline::Text(r) if r.text.contains("various parts"))
                        )
                    );
                    assert!(figure.captions[0].source.is_some());
                    println!("Figure 2.6: independent image and caption, section={index}");
                }
            }
        }
    }
    println!("captioned figures={figures}");
    assert!(found);
    assert!(figures >= 22);
}
