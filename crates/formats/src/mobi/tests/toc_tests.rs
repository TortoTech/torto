use super::*;
use rebook_publication::{TableOfContentsOrigin, TocEntry};

const TEXT: &[u8] = b"<html><body><p>Cover</p><mbp:pagebreak/><h2>ONE<br/>Heading &amp; details</h2><p>Main text</p><mbp:pagebreak/><p>Notes</p><p>Child notes</p></body></html>";

fn offset(needle: &[u8]) -> usize {
    TEXT.windows(needle.len())
        .position(|part| part == needle)
        .unwrap()
}

fn fixture_entries() -> Vec<(&'static str, usize, Option<usize>)> {
    vec![
        ("One: Heading &#x2014; details", offset(b"<h2>"), None),
        ("Footnotes", offset(b"<p>Notes"), None),
        ("Chapter 1", offset(b"<p>Child notes"), Some(1)),
    ]
}

#[test]
fn legacy_ncx_supplies_labels_hierarchy_and_exact_targets_without_changing_content() {
    let entries = fixture_entries();
    let bytes = mobi6_fixture_with_index(TEXT, &index_records(&entries));
    let source = open(&bytes, "fixture.mobi", BookFormat::Mobi).unwrap();
    assert_eq!(
        source.table_of_contents_origin(),
        TableOfContentsOrigin::Embedded
    );
    assert_eq!(source.book().sections.len(), 3);
    let toc = &source.book().table_of_contents;
    assert_eq!(toc.len(), 2);
    assert_eq!(toc[0].label, "One: Heading — details");
    assert_eq!(toc[1].label, "Footnotes");
    assert_eq!(toc[1].children.len(), 1);
    assert_eq!(toc[1].children[0].label, "Chapter 1");
    assert_ne!(toc[1].href, toc[1].children[0].href);
    assert_targets(&source, toc);

    let without_index = open(
        &mobi6_fixture_with_index(TEXT, &[]),
        "fixture.mobi",
        BookFormat::Mobi,
    )
    .unwrap();
    for i in 0..source.book().sections.len() {
        let texts = |source: &DirectBookSource| {
            source
                .parse_section(i)
                .unwrap()
                .blocks
                .iter()
                .flat_map(|block| {
                    block
                        .text_blocks()
                        .map(|text| (text.content.clone(), text.source.clone()))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&source), texts(&without_index));
    }
}

#[test]
fn legacy_ncx_accepts_alignment_padding_and_rejects_truncated_strings() {
    let entries = fixture_entries();
    let mut records = index_records(&entries);
    let unpadded = records[2].iter().rposition(|byte| *byte != 0).unwrap() + 1;
    records[2].truncate(unpadded);
    for padding in 0..=3 {
        let mut records = records.clone();
        records[2].extend(std::iter::repeat_n(0, padding));
        let source = open(
            &mobi6_fixture_with_index(TEXT, &records),
            "fixture.mobi",
            BookFormat::Mobi,
        )
        .unwrap();
        assert_eq!(
            source.table_of_contents_origin(),
            TableOfContentsOrigin::Embedded
        );
    }
    records[2].push(1); // Unterminated variable-length string length, not zero padding.
    let source = open(
        &mobi6_fixture_with_index(TEXT, &records),
        "fixture.mobi",
        BookFormat::Mobi,
    )
    .unwrap();
    assert_eq!(
        source.table_of_contents_origin(),
        TableOfContentsOrigin::Fallback
    );
    assert_eq!(source.book().sections.len(), 3);
}

#[test]
fn legacy_ncx_invalid_targets_do_not_hide_valid_entries_and_cycles_fall_back() {
    let mut entries = fixture_entries();
    entries[1].1 = TEXT.len() + 10;
    let source = open(
        &mobi6_fixture_with_index(TEXT, &index_records(&entries)),
        "fixture.mobi",
        BookFormat::Mobi,
    )
    .unwrap();
    assert_eq!(source.book().table_of_contents.len(), 2);
    assert_eq!(source.book().table_of_contents[1].label, "Chapter 1");
    assert_targets(&source, &source.book().table_of_contents);

    let mut entries = fixture_entries();
    entries[1].2 = Some(2); // Both ancestors are present, but the parent relation cycles.
    let source = open(
        &mobi6_fixture_with_index(TEXT, &index_records(&entries)),
        "fixture.mobi",
        BookFormat::Mobi,
    )
    .unwrap();
    assert_eq!(
        source.table_of_contents_origin(),
        TableOfContentsOrigin::Fallback
    );
    assert_eq!(source.book().sections.len(), 3);
}

#[test]
fn legacy_fallback_heading_keeps_break_spacing_and_decodes_entities() {
    let source = open(
        &mobi6_fixture_with_index(TEXT, &[]),
        "fixture.mobi",
        BookFormat::Mobi,
    )
    .unwrap();
    assert_eq!(
        source.book().table_of_contents[1].label,
        "ONE Heading & details"
    );
    let text = b"<h2><small>TWO</small><BR />What <em>Super</em>markets Don&#8217;t Want</h2>";
    let source = open(
        &mobi6_fixture_with_index(text, &[]),
        "fixture.mobi",
        BookFormat::Mobi,
    )
    .unwrap();
    assert_eq!(
        source.book().table_of_contents[0].label,
        "TWO What Supermarkets Don’t Want"
    );
}

#[test]
#[ignore = "requires TORTO_TEST_UNDERCOVER_MOBI pointing to the local reported book"]
fn local_undercover_economist_uses_embedded_navigation_and_resolves_every_target() {
    let path = std::env::var_os("TORTO_TEST_UNDERCOVER_MOBI").expect("TORTO_TEST_UNDERCOVER_MOBI");
    let bytes = std::fs::read(path).unwrap();
    let source = open(&bytes, "fixture.mobi", BookFormat::Mobi).unwrap();
    assert!(source.book().metadata.title.starts_with("The Undercover Economist"));
    assert_eq!(
        source.table_of_contents_origin(),
        TableOfContentsOrigin::Embedded
    );
    assert_eq!(source.book().sections.len(), 29);
    let toc = &source.book().table_of_contents;
    assert_eq!(toc.len(), 22);
    assert!(
        toc.iter()
            .any(|entry| entry.label == "One: Who Pays for Your Coffee?")
    );
    assert!(
        toc.iter()
            .any(|entry| entry.label == "Two: What Supermarkets Don’t Want You to Know")
    );
    assert_eq!(toc.last().unwrap().label, "Footnotes");
    assert_eq!(toc.last().unwrap().children.len(), 6);
    assert_targets(&source, toc);
    assert_eq!(
        (0..29)
            .map(|i| source.parse_section(i).unwrap().blocks.len())
            .sum::<usize>(),
        2033
    );
}

fn assert_targets(source: &DirectBookSource, entries: &[TocEntry]) {
    for entry in entries {
        let href = entry.href.as_ref().unwrap();
        let section_index = source
            .book()
            .sections
            .iter()
            .position(|item| item.href.path() == href.path())
            .unwrap();
        let section = source.parse_section(section_index).unwrap();
        assert!(
            section
                .anchors
                .iter()
                .any(|anchor| Some(anchor.fragment.as_str()) == href.fragment()),
            "{}: {href:?}",
            entry.label
        );
        assert_targets(source, &entry.children);
    }
}

fn index_records(entries: &[(&str, usize, Option<usize>)]) -> Vec<Vec<u8>> {
    fn vint(mut value: usize) -> Vec<u8> {
        let mut result = vec![(value as u8 & 0x7f) | 0x80];
        value >>= 7;
        while value != 0 {
            result.push(value as u8 & 0x7f);
            value >>= 7;
        }
        result.reverse();
        result
    }
    let mut strings = Vec::new();
    let mut offsets = Vec::new();
    let mut data = vec![0; 192];
    data[..4].copy_from_slice(b"INDX");
    put_u32(&mut data, 4, 192);
    put_u32(&mut data, 24, entries.len() as u32);
    for (i, (label, position, parent)) in entries.iter().enumerate() {
        let label_offset = strings.len();
        strings.extend(vint(label.len()));
        strings.extend_from_slice(label.as_bytes());
        offsets.push(data.len());
        let name = format!("{i:03}");
        data.push(name.len() as u8);
        data.extend_from_slice(name.as_bytes());
        data.push(if parent.is_some() { 0x0f } else { 0x07 });
        data.extend(vint(*position));
        data.extend(vint(label_offset));
        data.extend(vint(usize::from(parent.is_some())));
        if let Some(parent) = parent {
            data.extend(vint(*parent));
        }
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }
    let idxt = data.len();
    put_u32(&mut data, 20, idxt as u32);
    data.extend_from_slice(b"IDXT");
    for offset in offsets {
        data.extend_from_slice(&(offset as u16).to_be_bytes());
    }
    while strings.len() % 4 != 0 {
        strings.push(0);
    }

    let mut main = vec![0; 192];
    main[..4].copy_from_slice(b"INDX");
    put_u32(&mut main, 4, 192);
    put_u32(&mut main, 24, 1);
    put_u32(&mut main, 28, 65_001);
    put_u32(&mut main, 52, 1);
    main.extend_from_slice(b"TAGX");
    main.extend_from_slice(&32_u32.to_be_bytes());
    main.extend_from_slice(&1_u32.to_be_bytes());
    main.extend_from_slice(&[1, 1, 1, 0, 3, 1, 2, 0, 4, 1, 4, 0, 21, 1, 8, 0, 0, 0, 0, 1]);
    vec![main, data, strings]
}
