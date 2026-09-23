use super::*;

impl ShapedTextRegion {
    pub(super) fn citation_original(&self, range: Range<usize>) -> String {
        let mut result = String::new();
        let mut at = range.start;
        for c in self
            .citations
            .iter()
            .filter(|c| c.range.start < range.end && c.range.end > range.start)
        {
            if at < c.range.start {
                result.push_str(&self.text[at..c.range.start]);
            }
            let from = at.max(c.range.start);
            let to = range.end.min(c.range.end);
            let count = self.text[c.range.clone()].chars().count().max(1);
            let original = c.original.chars().count();
            let start = self.text[c.range.start..from].chars().count() * original / count;
            let end = self.text[c.range.start..to].chars().count() * original / count;
            result.extend(c.original.chars().skip(start).take(end - start));
            at = to;
        }
        if at < range.end {
            result.push_str(&self.text[at..range.end]);
        }
        result
    }

    pub(super) fn citation_display_byte(&self, offset: usize, end: bool) -> usize {
        let mut remaining = offset;
        let mut at = self.source_text_start;
        for c in self.citations.iter() {
            if c.range.end <= at {
                continue;
            }
            let plain = self.text[at..c.range.start].chars().count();
            if remaining < plain {
                return at + byte_index_for_char_offset(&self.text[at..c.range.start], remaining);
            }
            remaining -= plain;
            let length = c.original.chars().count();
            if remaining < length {
                return if remaining == 0 || !end {
                    c.range.start
                } else {
                    c.range.end
                };
            }
            remaining -= length;
            at = c.range.end;
        }
        at + byte_index_for_char_offset(&self.text[at..], remaining)
    }
}

impl PageDisplayList {
    /// Citation ordinal and paragraph source at a displayed reference icon.
    pub fn inline_citation_at(&self, x: f32, y: f32) -> Option<(SourceRange, u32)> {
        self.footnote_regions
            .iter()
            .rev()
            .find(|f| {
                f.citation_number > 0 && f.bounds.contains(Point::new(f64::from(x), f64::from(y)))
            })
            .map(|f| (f.source.clone(), f.citation_number))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::{LayoutEngine, LayoutViewport, ReaderFontBlob, ReaderStyle, SpreadMode};
    use rebook_publication::*;

    struct Source(Book);
    impl BookSource for Source {
        fn book(&self) -> &Book {
            &self.0
        }
        fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
            unreachable!()
        }
        fn resource(&self, _: &PublicationUrl) -> Result<Resource, PublicationError> {
            unreachable!()
        }
    }

    #[test]
    fn numbered_icons_preserve_copy_geometry_and_source_offsets() {
        let source = Source(Book {
            id: PublicationId::new("citations").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let mut content = Vec::new();
        for (value, number) in [
            ("Evidence ", 0),
            ("(Smith, ", 1),
            ("2020; Jones, 1990)", 1),
            (" and more ", 0),
            ("[2]", 123),
            (". Trailing text.", 0),
        ] {
            content.push(Inline::Text(TextRun {
                text: value.into(),
                style: TextStyle {
                    inline_citation: number,
                    ..Default::default()
                },
                link: None,
            }));
        }
        let original: String = content
            .iter()
            .filter_map(|i| match i {
                Inline::Text(r) => Some(r.text.as_str()),
                _ => None,
            })
            .collect();
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        let range = SourceRange {
            start: anchor.clone(),
            end: SourceAnchor {
                text_offset: original.chars().count() as u64,
                ..anchor
            },
        };
        let block = Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content,
            style: Default::default(),
            source: Some(range.clone()),
        });
        const FONT: &[u8] = include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf");
        let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(FONT))]);
        let style = ReaderStyle {
            spread: SpreadMode::Single,
            horizontal_margin: 12.0,
            top_margin: 12.0,
            bottom_margin: 12.0,
            ..ReaderStyle::default()
        };
        for width in [220, 600] {
            let result = engine
                .layout_blocks(
                    &source,
                    std::slice::from_ref(&block),
                    LayoutViewport::new(width, 160).unwrap(),
                    &style,
                )
                .unwrap();
            let mut copied = String::new();
            let mut ordinals = Vec::new();
            for page in result.pages {
                let display = DisplayListCompiler.compile(&page);
                for region in &display.text_regions {
                    let TextRegion::Shaped(region) = region else {
                        continue;
                    };
                    let bytes = region.visible_byte_range().unwrap();
                    copied.push_str(&region.selection_fragment(bytes).unwrap().quote);
                    for c in region.citations.iter() {
                        if !region.lines.clone().any(|i| {
                            region
                                .layout
                                .get(i)
                                .unwrap()
                                .text_range()
                                .contains(&c.range.start)
                        }) {
                            continue;
                        }
                        let selected = region
                            .selection_fragment(c.range.start..c.range.start + 1)
                            .unwrap();
                        assert_eq!(selected.quote, c.original);
                        assert_eq!(
                            region.byte_range_for_source(&selected.range),
                            Some(c.range.clone())
                        );
                    }
                }
                for icon in &display.footnote_regions {
                    assert!(!icon.citation_glyphs.is_empty());
                    assert!(icon.bounds.width() > 0.0 && icon.bounds.width() < 90.0);
                    assert_eq!(
                        display.inline_citation_at(
                            icon.bounds.center().x as f32,
                            icon.bounds.center().y as f32
                        ),
                        Some((range.clone(), icon.citation_number))
                    );
                    ordinals.push(icon.citation_number);
                }
            }
            assert_eq!(copied, original);
            assert_eq!(ordinals, vec![1, 123]);
        }
        let mut companion = block.clone();
        let Block::Text(t) = &mut companion else {
            panic!()
        };
        t.source = None;
        let bilingual = engine
            .layout_blocks(
                &source,
                &[block, companion],
                LayoutViewport::new(600, 1000).unwrap(),
                &style,
            )
            .unwrap();
        let display = DisplayListCompiler.compile(&bilingual.pages[0]);
        let icons = &display.footnote_regions;
        assert_eq!(icons.len(), 4);
        assert!(
            icons
                .iter()
                .all(|icon| icon.source == range && icon.bounds.height() < 40.0)
        );
        assert!(icons[0].bounds.y1 < icons[2].bounds.y0);
        let shifted = display.translate_source_text(&range, 8.0);
        assert!((shifted.footnote_regions[0].bounds.y0 - icons[0].bounds.y0 - 8.0).abs() < 0.01);
    }
}
