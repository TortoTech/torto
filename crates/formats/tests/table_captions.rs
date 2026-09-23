//! Local regression survey; copyrighted fixtures remain outside the repository.
use rebook_publication::{Block, TextBlockKind};

#[test]
#[ignore = "requires TORTO_TABLE_BOOK pointing to a local EPUB"]
fn local_table_caption_survey() {
    let path = std::env::var_os("TORTO_TABLE_BOOK").expect("TORTO_TABLE_BOOK");
    let opened = rebook_formats::open_file(std::path::PathBuf::from(path)).unwrap();
    let source = opened.source();
    let mut grids = 0;
    let mut before = 0;
    let mut after = 0;
    let mut notes = 0;
    for index in 0..source.book().sections.len() {
        let section = source.parse_section(index).unwrap();
        for block in &section.blocks {
            if let Block::Table(table) = block {
                grids += 1;
                before += table.before.len();
                after += table.after.len();
                notes += table
                    .before
                    .iter()
                    .chain(&table.after)
                    .filter(|text| text.kind == TextBlockKind::Paragraph)
                    .count();
                assert!(table.text_blocks().all(|text| text.source.is_some()));
            }
        }
    }
    println!("grids={grids} before={before} after={after} notes={notes}");
    assert!(grids > 0);
    if std::env::var_os("TORTO_TABLE_EXPECT_NO_CAPTIONS").is_some() {
        assert_eq!(before + after, 0);
    } else {
        assert!(before + after > 0);
    }
}
