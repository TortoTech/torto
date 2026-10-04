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
            .and_then(|f| f.source.clone().map(|source| (source, f.citation_number)))
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
    fn bilingual_notes_and_citations_keep_distinct_shared_marker_identities() {
        let source = Source(Book {
            id: PublicationId::new("bilingual-markers").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        let owner = SourceRange {
            start: anchor.clone(),
            end: SourceAnchor {
                text_offset: 20,
                ..anchor
            },
        };
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "Body ".into(),
                    style: TextStyle::default(),
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "note".into(),
                    style: TextStyle {
                        inline_role: InlineRole::Footnote,
                        ..Default::default()
                    },
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: " and ".into(),
                    style: TextStyle::default(),
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "Smith, 2020".into(),
                    style: TextStyle {
                        inline_citation: 1,
                        ..Default::default()
                    },
                    link: None,
                }),
            ],
            style: BlockStyle::default(),
            source: Some(owner.clone()),
        };
        let mut companion = block.clone();
        companion.source = None;
        companion.style.reference_owner = Some(source_block_identity(&owner));
        let style = ReaderStyle {
            spread: SpreadMode::Single,
            focus_footnote_icons: true,
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            ..Default::default()
        };
        let layout = LayoutEngine::new()
            .layout_blocks(
                &source,
                &[Block::Text(block), Block::Text(companion)],
                LayoutViewport::new(800, 600).unwrap(),
                &style,
            )
            .unwrap();
        let display = DisplayListCompiler.compile(&layout.pages[0]);
        // Focus-unit geometry may cover a subset of the source paragraph.
        // Both the original and source-less translated markers must paint.
        let mut partial = owner.clone();
        partial.start.text_offset = 3;
        partial.end.text_offset = 8;
        let yellow = Color::from_rgba8(250, 204, 21, 255);
        let normal = Color::from_rgba8(37, 99, 235, 255);
        let mut actual = anyrender::Scene::new();
        display.paint_focus_footnote_icons(
            &mut actual,
            std::slice::from_ref(&partial),
            normal,
            Some((&owner, 0x2000_0001, yellow)),
            17.0,
        );
        display.paint_focus_footnote_activation_bars(
            &mut actual,
            std::slice::from_ref(&partial),
            Some((&owner, 0x2000_0001, yellow)),
            17.0,
        );
        let mut expected = anyrender::Scene::new();
        assert_eq!(display.footnote_regions.len(), 4);
        for region in &display.footnote_regions {
            paint_footnote_region(
                &mut expected,
                region,
                if region.reference_number == 0x2000_0001 {
                    yellow
                } else {
                    normal
                },
                Affine::translate((17.0, 0.0)),
            );
        }
        for region in &display.footnote_regions {
            if region.reference_number == 0x2000_0001 {
                let y = region.activation_bar_y;
                assert!(
                    y > region.bounds.y1,
                    "bar belongs below the body line, not the superscript"
                );
                paint_footnote_activation_bar(
                    &mut expected,
                    region,
                    yellow,
                    Affine::translate((17.0, 0.0)),
                );
            }
        }
        assert!(!actual.commands.is_empty());
        assert_eq!(
            actual.commands, expected.commands,
            "paint each marker once and add a bar only to the active original and translated references"
        );
        for number in [1, 0x2000_0001] {
            let bounds = display.footnote_reference_bounds(&owner, number);
            assert_eq!(
                bounds.len(),
                2,
                "both language markers must retain their association"
            );
            for bound in bounds {
                assert_eq!(
                    display.footnote_reference_at(
                        (bound[0] + bound[2]) * 0.5,
                        (bound[1] + bound[3]) * 0.5
                    ),
                    Some((source_block_identity(&owner), number))
                );
            }
        }
    }

    #[test]
    fn generated_text_math_preserves_copy_and_source_offsets() {
        let source = Source(Book {
            id: PublicationId::new("text-math").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let original = "Before N ~ I0.301 after.";
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
        let run = |text: &str| TextRun {
            text: text.into(),
            style: Default::default(),
            link: None,
        };
        let block = Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            style: Default::default(),
            source: Some(range.clone()),
            content: vec![
                Inline::Text(run("Before ")),
                Inline::Math(MathRun {
                    original: Some(vec![run("N ~ I0.301")]),
                    latex: r"N \sim I^{0.301}".into(),
                    display: false,
                    size_scale: 1.0,
                }),
                Inline::Text(run(" after.")),
            ],
        });
        let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(
            include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice(),
        ))]);
        for unified in [false, true] {
            let style = ReaderStyle {
                typesetting: if unified {
                    rebook_layout::ReaderTypesetting::unified()
                } else {
                    Default::default()
                },
                spread: SpreadMode::Single,
                ..Default::default()
            };
            let result = engine
                .layout_blocks(
                    &source,
                    std::slice::from_ref(&block),
                    LayoutViewport::new(800, 400).unwrap(),
                    &style,
                )
                .unwrap();
            let mut copied = String::new();
            for page in result.pages {
                let display = DisplayListCompiler.compile(&page);
                for region in &display.text_regions {
                    if let TextRegion::Shaped(region) = region {
                        if let Some(fragment) = region
                            .visible_byte_range()
                            .and_then(|range| region.selection_fragment(range))
                        {
                            copied.push_str(&fragment.quote);
                            assert!(fragment.range.end.text_offset <= range.end.text_offset);
                        }
                    }
                }
            }
            assert_eq!(copied, original, "unified={unified}");
        }
    }

    #[test]
    fn reshaped_hyphenated_ligatures_preserve_copied_text() {
        let source = Source(Book {
            id: PublicationId::new("ligature-copy").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let original="The efficient office studies diffraction and difficult scientific effects. Officials offer sufficient information about artificial interference and different reflections. ".repeat(5);
        let start = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        let range = SourceRange {
            end: SourceAnchor {
                text_offset: original.chars().count() as u64,
                ..start.clone()
            },
            start,
        };
        let block = Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: original.clone(),
                style: TextStyle {
                    language: TextLanguage::EnglishUs,
                    ..Default::default()
                },
                link: None,
            })],
            style: Default::default(),
            source: Some(range),
        });
        let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(
            include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice(),
        ))]);
        let style = ReaderStyle {
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            spread: SpreadMode::Single,
            horizontal_margin: 0.,
            ..Default::default()
        };
        for width in [280, 800] {
            let layout = engine
                .layout_blocks(
                    &source,
                    std::slice::from_ref(&block),
                    LayoutViewport::new(width, 400).unwrap(),
                    &style,
                )
                .unwrap();
            let mut copied = String::new();
            for page in layout.pages {
                let display = DisplayListCompiler.compile(&page);
                for region in &display.text_regions {
                    if let TextRegion::Shaped(region) = region {
                        if let Some(fragment) = region
                            .visible_byte_range()
                            .and_then(|range| region.selection_fragment(range))
                        {
                            copied.push_str(&fragment.quote);
                        }
                    }
                }
            }
            assert_eq!(copied, original);
        }
    }

    #[test]
    fn website_icons_keep_targets_and_copy_text_in_unified_layout() {
        let source = Source(Book {
            id: PublicationId::new("web").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let original =
            "Visit example.com/path?q=1 and www.example.org. Mail a@example.com. Value 3.14.";
        let start = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        let range = SourceRange {
            end: SourceAnchor {
                text_offset: original.chars().count() as u64,
                ..start.clone()
            },
            start,
        };
        let block = Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: original.into(),
                style: Default::default(),
                link: None,
            })],
            style: Default::default(),
            source: Some(range),
        });
        let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(
            include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice(),
        ))]);
        let mut style = ReaderStyle {
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            spread: SpreadMode::Single,
            ..Default::default()
        };
        for unified in [true, false] {
            if !unified {
                style.typesetting = Default::default();
            }
            let result = engine
                .layout_blocks(
                    &source,
                    std::slice::from_ref(&block),
                    LayoutViewport::new(800, 400).unwrap(),
                    &style,
                )
                .unwrap();
            let mut urls = Vec::new();
            let mut copied = String::new();
            for page in result.pages {
                let display = DisplayListCompiler.compile(&page);
                for region in &display.text_regions {
                    if let TextRegion::Shaped(region) = region {
                        copied.push_str(
                            &region
                                .selection_fragment(region.visible_byte_range().unwrap())
                                .unwrap()
                                .quote,
                        );
                    }
                }
                for icon in &display.footnote_regions {
                    if let Some(url) = &icon.website {
                        urls.push(url.clone());
                        let point = icon.bounds.center();
                        assert_eq!(
                            display.website_at(point.x as f32, point.y as f32),
                            Some(url.clone())
                        );
                        assert!(
                            display
                                .footnote_source_at(point.x as f32, point.y as f32)
                                .is_none()
                        );
                        assert!(
                            display
                                .inline_citation_at(point.x as f32, point.y as f32)
                                .is_none()
                        );
                    }
                }
            }
            assert_eq!(copied, original);
            assert_eq!(urls.len(), if unified { 2 } else { 0 });
            if unified {
                assert_eq!(urls[0], "https://example.com/path?q=1");
            }
        }
    }

    #[test]
    fn numbered_footnotes_keep_popup_hits_and_original_copy() {
        let source = Source(Book {
            id: PublicationId::new("numbered-notes").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        let original = "Body hidden note after";
        let range = SourceRange {
            start: anchor.clone(),
            end: SourceAnchor {
                text_offset: original.len() as u64,
                ..anchor
            },
        };
        let block = Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: [("Body ", false), ("hidden note", true), (" after", false)]
                .into_iter()
                .map(|(text, note)| {
                    Inline::Text(TextRun {
                        text: text.into(),
                        style: TextStyle {
                            inline_role: if note {
                                rebook_publication::InlineRole::Footnote
                            } else {
                                rebook_publication::InlineRole::Normal
                            },
                            ..Default::default()
                        },
                        link: None,
                    })
                })
                .collect(),
            style: Default::default(),
            source: Some(range.clone()),
        });
        let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(include_bytes!(
            "../../../assets/fonts/Literata-opsz-wght.ttf"
        )))]);
        let style = ReaderStyle {
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            focus_footnote_icons: true,
            spread: SpreadMode::Single,
            ..Default::default()
        };
        let result = engine
            .layout_blocks(
                &source,
                &[block],
                LayoutViewport::new(600, 800).unwrap(),
                &style,
            )
            .unwrap();
        let display = DisplayListCompiler.compile(&result.pages[0]);
        assert_eq!(display.footnote_regions.len(), 1);
        let icon = &display.footnote_regions[0];
        assert!(!icon.citation_glyphs.is_empty());
        assert_eq!(icon.citation_number, 0);
        let body_baseline = result.pages[0]
            .items
            .iter()
            .find_map(|item| {
                let rebook_layout::PageItem::Text(text) = item else {
                    return None;
                };
                Some(text.layout.get(text.lines.start)?.metrics().baseline)
            })
            .unwrap();
        let marker_baseline = icon.citation_glyphs[0].glyphs[0].y;
        assert!(
            (body_baseline - marker_baseline - style.typography.font_size * 0.35).abs() < 0.1,
            "superscript must rise relative to body size, not the small marker size"
        );
        let center = icon.bounds.center();
        assert_eq!(
            display.footnote_source_at(center.x as f32, center.y as f32),
            Some(range)
        );
        assert_eq!(
            display.inline_citation_at(center.x as f32, center.y as f32),
            None
        );
        let copied: String = display
            .text_regions
            .iter()
            .filter_map(|region| {
                let TextRegion::Shaped(region) = region else {
                    return None;
                };
                Some(
                    region
                        .selection_fragment(region.visible_byte_range()?)
                        .unwrap()
                        .quote,
                )
            })
            .collect();
        assert_eq!(copied, original);
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
                .all(|icon| icon.source.as_ref() == Some(&range) && icon.bounds.height() < 40.0)
        );
        assert!(icons[0].bounds.y1 < icons[2].bounds.y0);
        let shifted = display.translate_source_text(&range, 8.0);
        assert!((shifted.footnote_regions[0].bounds.y0 - icons[0].bounds.y0 - 8.0).abs() < 0.01);
    }
}
