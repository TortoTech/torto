//! Recover body flow without making floating illustrations part of the text.
use super::*;
use kurbo::Rect;

pub(super) type Floats = HashMap<String, (usize, Rect)>;

/// Use the extraction identity, including all subimages, instead of estimating
/// whether one original image occupies most of an already combined crop.
pub(super) fn register_floats(
    stored: &StoredSection,
    crops: &[layout::Crop],
    page: usize,
    floats: &mut Floats,
) {
    for crop in crops.iter().filter(|crop| !crop.image_sources.is_empty()) {
        floats.insert(crop.path.clone(), (page, crop.bounds));
    }
    for block in &stored.section.blocks {
        let Block::Figure(figure) = block else {
            continue;
        };
        let nodes = figure
            .captions
            .iter()
            .filter_map(|c| c.source.as_ref())
            .map(|s| s.start.node.as_str())
            .collect::<HashSet<_>>();
        let caption = stored
            .provenance
            .iter()
            .filter(|s| nodes.contains(s.source.start.node.as_str()))
            .map(|s| Rect::new(s.rect[0], s.rect[1], s.rect[2], s.rect[3]))
            .reduce(|a, b| a.union(b));
        if let Some(caption) = caption {
            for image in &figure.images {
                if let Some((_, bounds)) = floats.get_mut(image.href.path()) {
                    *bounds = bounds.union(caption);
                }
            }
        }
    }
}

fn paragraph(block: &Block) -> Option<&TextBlock> {
    match block {
        Block::Text(t) if t.kind == TextBlockKind::Paragraph => Some(t),
        _ => None,
    }
}

fn plain(text: &TextBlock) -> String {
    text.content
        .iter()
        .filter_map(|i| match i {
            Inline::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect()
}

fn floating(block: &Block, floats: &Floats) -> Option<(usize, Rect)> {
    // Only actual PDF images with a caption qualify. Formula and page/region
    // fallbacks must remain reading-order barriers.
    let Block::Figure(figure) = block else {
        return None;
    };
    if figure.captions.is_empty() || figure.images.is_empty() {
        return None;
    }
    let mut bounds: Option<(usize, Rect)> = None;
    for image in &figure.images {
        if image.formula_image {
            return None;
        }
        let &(page, rect) = floats.get(image.href.path())?;
        bounds = Some(match bounds {
            Some((p, r)) if p == page => (p, r.union(rect)),
            None => (page, rect),
            _ => return None,
        });
    }
    bounds
}

fn row(provenance: &[SourceSlice], node: &str, tail: bool, body: f64) -> Option<(usize, Rect)> {
    let slice = if tail {
        provenance
            .iter()
            .rev()
            .find(|p| p.source.start.node == node)
    } else {
        provenance.iter().find(|p| p.source.start.node == node)
    }?;
    let rect = provenance
        .iter()
        .filter(|p| {
            p.page == slice.page
                && p.source.start.node == node
                && (p.rect[1] - slice.rect[1]).abs() < body * 0.2
        })
        .map(|p| Rect::new(p.rect[0], p.rect[1], p.rect[2], p.rect[3]))
        .reduce(|a, b| a.union(b))?;
    Some((slice.page, rect))
}

fn compatible(old: &TextBlock, new: &TextBlock) -> bool {
    let left = plain(old);
    let right = plain(new);
    if !crate::reflow::continues(&left, &right, crate::reflow::ContinuationEvidence::Geometry) {
        return false;
    }
    let size = |t: &TextBlock, tail: bool| {
        let mut runs = t.content.iter().filter_map(|i| match i {
            Inline::Text(r)
                if r.style.baseline == TextBaseline::Normal
                    && r.text.chars().any(char::is_alphabetic) =>
            {
                Some(r.style.size_scale)
            }
            _ => None,
        });
        if tail { runs.next_back() } else { runs.next() }
    };
    size(old, true)
        .zip(size(new, false))
        .is_some_and(|(a, b)| (a - b).abs() <= 0.035)
}

/// Merge one text block and move its provenance/anchors into the owning section.
/// Inline links retain their original destination (e.g. a note on the next page).
fn merge(
    previous: &mut StoredSection,
    old_index: usize,
    next: &mut StoredSection,
    new_index: usize,
) {
    let Block::Text(first) = next.section.blocks.remove(new_index) else {
        unreachable!()
    };
    let Block::Text(last) = &mut previous.section.blocks[old_index] else {
        unreachable!()
    };
    let new_node = first.source.as_ref().unwrap().start.node.clone();
    let movement = crate::reflow::append_text(last, first).unwrap();
    let offset = movement.to.text_offset;
    let old_node = movement.to.node.clone();
    let target = movement.to.spine.clone();
    let mut moved = StoredSection {
        section: Section {
            blocks: next.section.blocks.drain(..new_index).collect(),
            anchors: Vec::new(),
            id: next.section.id.clone(),
            href: next.section.href.clone(),
        },
        provenance: Vec::new(),
    };
    let mut nodes = HashSet::from([new_node.clone()]);
    for block in &moved.section.blocks {
        if let Block::Figure(f) = block {
            for t in &f.captions {
                if let Some(s) = &t.source {
                    nodes.insert(s.start.node.clone());
                }
            }
            for i in &f.images {
                if let Some(s) = &i.source {
                    nodes.insert(s.start.node.clone());
                }
            }
        }
    }
    next.provenance.retain_mut(|slice| {
        if !nodes.contains(&slice.source.start.node) {
            return true;
        }
        if slice.source.start.node == new_node {
            slice.source.start.node = old_node.clone();
            slice.source.end.node = old_node.clone();
            slice.source.start.text_offset += offset;
            slice.source.end.text_offset += offset;
        }
        moved.provenance.push(slice.clone());
        false
    });
    next.section.anchors.retain_mut(|anchor| {
        if !nodes.contains(&anchor.source.node) {
            return true;
        }
        if anchor.source.node == new_node {
            anchor.source.node = old_node.clone();
            anchor.source.text_offset += offset;
        }
        moved.section.anchors.push(anchor.clone());
        false
    });
    // Suppress href rewriting: only source ownership changes during this move.
    moved.section.href = previous.section.href.clone();
    relocate(&mut moved, target, previous.section.href.clone());
    previous.section.blocks.extend(moved.section.blocks);
    previous.section.anchors.extend(moved.section.anchors);
    previous.provenance.extend(moved.provenance);
}

pub(super) fn across_pages(
    previous: &mut StoredSection,
    next: &mut StoredSection,
    body: f64,
    old_frame: Option<layout::BodyFrame>,
    new_frame: Option<layout::BodyFrame>,
    floats: &Floats,
) {
    let Some(old_index) = previous
        .section
        .blocks
        .iter()
        .rposition(|b| !matches!(b, Block::Note(_)) && floating(b, floats).is_none())
    else {
        return;
    };
    let Some(new_index) = next
        .section
        .blocks
        .iter()
        .position(|b| !matches!(b, Block::Note(_)) && floating(b, floats).is_none())
    else {
        return;
    };
    let (Some(old), Some(new)) = (
        paragraph(&previous.section.blocks[old_index]),
        paragraph(&next.section.blocks[new_index]),
    ) else {
        return;
    };
    if !compatible(old, new) {
        return;
    }
    let (Some(a), Some(b)) = (&old.source, &new.source) else {
        return;
    };
    let (Some((ap, ar)), Some((bp, br)), Some((af, bf))) = (
        row(&previous.provenance, &a.start.node, true, body),
        row(&next.provenance, &b.start.node, false, body),
        old_frame.zip(new_frame),
    ) else {
        return;
    };
    if bp != ap + 1
        || ((af.right - af.left) - (bf.right - bf.left)).abs() > body * 1.5
        || ((ar.x0 - af.left) - (br.x0 - bf.left)).abs() > body * 0.8
        || (ar.x1 - af.right).abs() > body * 2.0
    {
        return;
    }
    let old_floats = previous.section.blocks[old_index + 1..]
        .iter()
        .filter_map(|b| floating(b, floats))
        .collect::<Vec<_>>();
    let new_floats = next.section.blocks[..new_index]
        .iter()
        .filter_map(|b| floating(b, floats))
        .collect::<Vec<_>>();
    if old_floats
        .iter()
        .any(|(p, r)| *p > ap || (*p == ap && r.y0 < ar.y1 - body && r.y1 > ar.y0))
        || new_floats
            .iter()
            .any(|(p, r)| *p != bp || r.y1 > br.y0 + body)
    {
        return;
    }
    let old_bottom = old_floats
        .iter()
        .filter(|(p, r)| *p == ap && r.y0 >= ar.y1 - body)
        .map(|(_, r)| r.y1)
        .fold(ar.y1, f64::max);
    let new_top = new_floats
        .iter()
        .filter(|(p, r)| *p == bp && r.y1 <= br.y0 + body)
        .map(|(_, r)| r.y0)
        .fold(br.y0, f64::min);
    if old_bottom < af.height * 0.75 || new_top > bf.height * 0.25 {
        return;
    }
    // Notes are trailing metadata, never move their definitions into another chunk.
    if next.section.blocks[..new_index]
        .iter()
        .any(|b| matches!(b, Block::Note(_)))
    {
        return;
    }
    merge(previous, old_index, next, new_index);
}

pub(super) fn same_page(stored: &mut StoredSection, body: f64, floats: &Floats) {
    let mut i = 0;
    while i < stored.section.blocks.len() {
        let Some(old) = paragraph(&stored.section.blocks[i]) else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < stored.section.blocks.len()
            && floating(&stored.section.blocks[j], floats).is_some()
        {
            j += 1;
        }
        if j == i + 1 {
            i += 1;
            continue;
        }
        let Some(new) = stored.section.blocks.get(j).and_then(paragraph) else {
            i += 1;
            continue;
        };
        let (Some(a), Some(b)) = (&old.source, &new.source) else {
            i += 1;
            continue;
        };
        let rows = row(&stored.provenance, &a.start.node, true, body).zip(row(
            &stored.provenance,
            &b.start.node,
            false,
            body,
        ));
        let good = compatible(old, new)
            && rows.is_some_and(|((ap, ar), (bp, br))| {
                ap == bp
                    && ar.width() >= br.width() * 0.72
                    && (ar.x0 - br.x0).abs() <= body * 0.8
                    && stored.section.blocks[i + 1..j]
                        .iter()
                        .any(|b| floating(b, floats).is_some_and(|(_, r)| r.y0 >= ar.y1 - body))
                    && stored.section.blocks[i + 1..j].iter().all(|b| {
                        floating(b, floats).is_some_and(|(p, r)| {
                            p == ap
                                && (r.y1 <= ar.y0 || (r.y0 >= ar.y1 - body && r.y1 <= br.y0 + body))
                                && r.x1 >= ar.x0
                                && r.x0 <= ar.x1
                        })
                    })
            });
        if !good {
            i += 1;
            continue;
        }
        let node = b.start.node.clone();
        let mut next = StoredSection {
            section: Section {
                blocks: stored.section.blocks.drain(i + 1..=j).collect(),
                anchors: Vec::new(),
                id: stored.section.id.clone(),
                href: stored.section.href.clone(),
            },
            provenance: Vec::new(),
        };
        stored.provenance.retain(|p| {
            if p.source.start.node == node {
                next.provenance.push(p.clone());
                false
            } else {
                true
            }
        });
        stored.section.anchors.retain(|a| {
            if a.source.node == node {
                next.section.anchors.push(a.clone());
                false
            } else {
                true
            }
        });
        merge(stored, i, &mut next, j - i - 1);
        // Keep the figures immediately after the now complete paragraph.
        let appended = stored
            .section
            .blocks
            .split_off(stored.section.blocks.len() - (j - i - 1));
        stored.section.blocks.splice(i + 1..i + 1, appended);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(
        node: &str,
        value: &str,
        page: usize,
        rect: Rect,
        section: &mut StoredSection,
    ) -> Block {
        let start = SourceAnchor {
            spine: section.section.id.clone(),
            node: node.into(),
            text_offset: 0,
        };
        let mut end = start.clone();
        end.text_offset = value.chars().count() as u64;
        let source = SourceRange { start, end };
        section.provenance.push(SourceSlice {
            source: source.clone(),
            page,
            rect: [rect.x0, rect.y0, rect.x1, rect.y1],
            original_glyph: section.provenance.len(),
        });
        Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: value.into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: BlockStyle::default(),
            source: Some(source),
        })
    }

    fn section(index: usize) -> StoredSection {
        StoredSection {
            section: Section {
                id: spine_id(index).unwrap(),
                href: section_href(index).unwrap(),
                blocks: Vec::new(),
                anchors: Vec::new(),
            },
            provenance: Vec::new(),
        }
    }

    fn figure(section: &mut StoredSection, page: usize) -> Block {
        let suffix = section.provenance.len();
        let Block::Text(mut caption) = text(
            &format!("caption-{page}-{suffix}"),
            "Figure 1. An image.",
            page,
            Rect::new(100.0, 310.0, 500.0, 320.0),
            section,
        ) else {
            unreachable!()
        };
        caption.kind = TextBlockKind::Caption;
        let source = SourceRange {
            start: SourceAnchor {
                spine: section.section.id.clone(),
                node: format!("image-{page}-{suffix}"),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: section.section.id.clone(),
                node: format!("image-{page}-{suffix}"),
                text_offset: 0,
            },
        };
        Block::Figure(FigureBlock {
            images: vec![ImageBlock {
                formula_image: false,
                formula: None,
                href: PublicationUrl::parse("resources/image.png").unwrap(),
                alt: String::new(),
                style: ImageStyle::default(),
                source: Some(source.clone()),
                text_layer: None,
            }],
            captions: vec![caption],
            caption_position: CaptionPosition::After,
            style: BlockStyle::default(),
            source: Some(source),
        })
    }

    fn floats(page: usize) -> Floats {
        HashMap::from([(
            "resources/image.png".into(),
            (page, Rect::new(100.0, 100.0, 500.0, 300.0)),
        )])
    }

    #[test]
    fn registration_uses_picture_sources_and_preserves_recovery_barriers() {
        let mut stored = section(0);
        let image = figure(&mut stored, 1);
        stored.section.blocks.push(image);
        let crops = [
            layout::Crop {
                path: "resources/image.png".into(),
                bounds: Rect::new(100.0, 100.0, 500.0, 300.0),
                image_sources: vec![0, 1],
            },
            layout::Crop {
                path: "resources/recovery.png".into(),
                bounds: Rect::new(100.0, 400.0, 500.0, 600.0),
                image_sources: Vec::new(),
            },
        ];
        let mut geometry = Floats::new();
        register_floats(&stored, &crops, 1, &mut geometry);
        assert_eq!(
            floating(&stored.section.blocks[0], &geometry).unwrap().1.y1,
            320.0
        );
        assert!(!geometry.contains_key("resources/recovery.png"));
        if let Block::Figure(f) = &mut stored.section.blocks[0] {
            f.images[0].href = PublicationUrl::parse("resources/recovery.png").unwrap();
        }
        assert!(floating(&stored.section.blocks[0], &geometry).is_none());
    }

    #[test]
    fn floating_figure_rejoins_body_and_retains_caption_and_unicode_offsets() {
        let mut stored = section(0);
        let old = text(
            "old",
            "正文延续",
            1,
            Rect::new(100.0, 70.0, 500.0, 80.0),
            &mut stored,
        );
        let image = figure(&mut stored, 1);
        let new = text(
            "new",
            "到图后的文字。",
            1,
            Rect::new(100.0, 340.0, 500.0, 350.0),
            &mut stored,
        );
        stored.section.blocks = vec![old, image, new];
        stored.section.anchors.push(SectionAnchor {
            fragment: "continued".into(),
            source: stored.provenance.last().unwrap().source.start.clone(),
        });
        let count = stored.provenance.len();
        same_page(&mut stored, 10.0, &floats(1));
        assert_eq!(stored.section.blocks.len(), 2);
        assert_eq!(
            plain(paragraph(&stored.section.blocks[0]).unwrap()),
            "正文延续到图后的文字。"
        );
        assert!(
            matches!(&stored.section.blocks[1], Block::Figure(f) if plain(&f.captions[0]) == "Figure 1. An image.")
        );
        assert_eq!(stored.provenance.len(), count);
        let moved = stored
            .provenance
            .iter()
            .find(|p| p.source.start.node == "old" && p.source.start.text_offset > 0)
            .unwrap();
        assert_eq!(moved.source.start.text_offset, 4);
        assert_eq!(stored.section.anchors[0].source.text_offset, 4);
    }

    #[test]
    fn floating_join_rejects_completed_paragraph_indent_font_and_formula() {
        for case in 0..7 {
            let mut stored = section(0);
            let old = text(
                "old",
                if case == 5 {
                    "The sentence ends.”"
                } else if case == 0 {
                    "The sentence ends."
                } else {
                    "The interface"
                },
                1,
                Rect::new(100.0, 70.0, 500.0, 80.0),
                &mut stored,
            );
            let mut image = figure(&mut stored, 1);
            if case == 3
                && let Block::Figure(f) = &mut image
            {
                f.images[0].formula_image = true;
            }
            let mut new = text(
                "new",
                "continues here.",
                1,
                Rect::new(if case == 1 { 120.0 } else { 100.0 }, 340.0, 500.0, 350.0),
                &mut stored,
            );
            if case == 2
                && let Block::Text(t) = &mut new
                && let Inline::Text(r) = &mut t.content[0]
            {
                r.style.size_scale = 0.8;
            }
            if case == 4
                && let Block::Text(t) = &mut new
            {
                t.kind = TextBlockKind::Heading(2);
            }
            if case == 6
                && let Block::Text(t) = &mut new
            {
                t.kind = TextBlockKind::ListItem {
                    ordered: true,
                    ordinal: 1,
                    depth: 0,
                    marker_visible: true,
                };
            }
            stored.section.blocks = vec![old, image, new];
            same_page(&mut stored, 10.0, &floats(1));
            assert_eq!(stored.section.blocks.len(), 3, "case {case}");
        }
    }

    #[test]
    fn moved_figure_does_not_block_the_next_page_of_the_same_paragraph() {
        let mut old = section(0);
        let first = text(
            "old",
            "The paragraph continues",
            1,
            Rect::new(100.0, 760.0, 500.0, 770.0),
            &mut old,
        );
        let image = figure(&mut old, 1);
        old.section.blocks = vec![first, image];
        let mut next = section(1);
        let tail = text(
            "new",
            "over a second page",
            2,
            Rect::new(100.0, 70.0, 500.0, 80.0),
            &mut next,
        );
        next.section.blocks.push(tail);
        let frame = Some(layout::BodyFrame {
            height: 800.0,
            left: 100.0,
            right: 500.0,
        });
        across_pages(&mut old, &mut next, 10.0, frame, frame, &floats(1));
        assert!(next.section.blocks.is_empty());
        assert_eq!(
            plain(paragraph(&old.section.blocks[0]).unwrap()),
            "The paragraph continues over a second page"
        );
        assert!(matches!(old.section.blocks[1], Block::Figure(_)));
    }

    #[test]
    fn successive_floating_figures_keep_one_complete_paragraph() {
        let mut stored = section(0);
        let first = text(
            "first",
            "The interface",
            1,
            Rect::new(100.0, 70.0, 500.0, 80.0),
            &mut stored,
        );
        let image1 = figure(&mut stored, 1);
        let middle = text(
            "middle",
            "continues between images",
            1,
            Rect::new(100.0, 340.0, 500.0, 350.0),
            &mut stored,
        );
        let mut image2 = figure(&mut stored, 1);
        if let Block::Figure(f) = &mut image2 {
            f.images[0].href = PublicationUrl::parse("resources/second.png").unwrap();
        }
        stored.provenance.last_mut().unwrap().rect = [100.0, 610.0, 500.0, 620.0];
        let last = text(
            "last",
            "and ends here.",
            1,
            Rect::new(100.0, 640.0, 500.0, 650.0),
            &mut stored,
        );
        stored.section.blocks = vec![first, image1, middle, image2, last];
        let mut geometry = floats(1);
        geometry.insert(
            "resources/second.png".into(),
            (1, Rect::new(100.0, 400.0, 500.0, 620.0)),
        );
        same_page(&mut stored, 10.0, &geometry);
        assert_eq!(stored.section.blocks.len(), 3);
        assert_eq!(
            plain(paragraph(&stored.section.blocks[0]).unwrap()),
            "The interface continues between images and ends here."
        );
        assert!(matches!(stored.section.blocks[1], Block::Figure(_)));
        assert!(matches!(stored.section.blocks[2], Block::Figure(_)));
        assert_eq!(stored.provenance.len(), 5);
    }

    #[test]
    #[ignore = "requires TORTO_TEST_NATIVE_GENERATION for a regenerated Pick, Click, Flick! cache"]
    fn local_native_floating_paragraphs() {
        let directory = PathBuf::from(std::env::var("TORTO_TEST_NATIVE_GENERATION").unwrap());
        let manifest: Manifest = read_json(&directory.join("manifest.json")).unwrap();
        let source = ReflowSource::open(&directory, manifest.book.id.as_str()).unwrap();
        let mut found = HashSet::new();
        for index in 0..source.book().sections.len() {
            let stored = source.stored(index).unwrap();
            for (i, block) in stored.section.blocks.iter().enumerate() {
                let Block::Figure(figure) = block else {
                    continue;
                };
                let caption = figure.captions.iter().map(plain).collect::<String>();
                let caption = caption.split_whitespace().collect::<Vec<_>>().join(" ");
                let case = if caption.starts_with("Figure 2.8 ") {
                    8
                } else if caption.starts_with("Figure 2.24 ") {
                    24
                } else if caption.starts_with("Figure 2.10 ") {
                    210
                } else if caption.starts_with("Figure 3.9 ") {
                    309
                } else if caption.starts_with("Figure 3.11 ") {
                    311
                } else if caption.starts_with("Figure 3.18 ") {
                    318
                } else if caption.starts_with("Figure 5.12 ") {
                    512
                } else {
                    continue;
                };
                let body = stored.section.blocks[..i]
                    .iter()
                    .rev()
                    .find(|b| !matches!(b, Block::Note(_)))
                    .and_then(paragraph)
                    .unwrap();
                let text = plain(body).split_whitespace().collect::<Vec<_>>().join(" ");
                if case == 8 {
                    assert!(text.contains("The user interface for the TX-2"), "{text}");
                } else if case == 24 {
                    assert!(
                        text.contains("was first demonstrated by Ivan Sutherland"),
                        "{text}"
                    );
                    let node = &body.source.as_ref().unwrap().start.node;
                    let pages = stored
                        .provenance
                        .iter()
                        .filter(|p| &p.source.start.node == node)
                        .map(|p| p.page)
                        .collect::<HashSet<_>>();
                    assert!(pages.contains(&119) && pages.contains(&120));
                    assert!(
                        stored
                            .section
                            .anchors
                            .iter()
                            .any(|a| a.fragment == "pdf-page-120")
                    );
                }
                if case >= 210 {
                    let expected = match case {
                        210 => "My Smalltalk code ran so slowly",
                        309 => "it had a full set of gestural commands",
                        311 => "object-oriented programming model",
                        318 => "make sure it was comfortable.",
                        512 => "thumb gets to where the cursor is",
                        _ => unreachable!(),
                    };
                    assert!(text.contains(expected), "case {case}: {text}");
                }
                found.insert(case);
            }
        }
        assert_eq!(found, HashSet::from([8, 24, 210, 309, 311, 318, 512]));
    }

    #[test]
    fn continuation_crosses_storage_cut_without_changing_note_links() {
        let mut old = section(0);
        let first = text(
            "old",
            "The interface",
            1,
            Rect::new(100.0, 760.0, 500.0, 770.0),
            &mut old,
        );
        old.section.blocks.push(first);
        let mut next = section(1);
        let image = figure(&mut next, 2);
        let mut continuation = text(
            "new",
            "continues on the next page.",
            2,
            Rect::new(80.0, 340.0, 480.0, 350.0),
            &mut next,
        );
        let link = next.section.href.resolve("#note").unwrap();
        if let Block::Text(t) = &mut continuation
            && let Inline::Text(r) = &mut t.content[0]
        {
            r.link = Some(link.clone());
        }
        next.section.anchors.push(SectionAnchor {
            fragment: "page".into(),
            source: next.provenance.last().unwrap().source.start.clone(),
        });
        let following = text(
            "following",
            "A separate paragraph.",
            2,
            Rect::new(100.0, 380.0, 480.0, 390.0),
            &mut next,
        );
        next.section.blocks = vec![image, continuation, following];
        let count = old.provenance.len() + next.provenance.len();
        across_pages(
            &mut old,
            &mut next,
            10.0,
            Some(layout::BodyFrame {
                height: 800.0,
                left: 100.0,
                right: 500.0,
            }),
            Some(layout::BodyFrame {
                height: 800.0,
                left: 80.0,
                right: 480.0,
            }),
            &floats(2),
        );
        assert_eq!(
            plain(paragraph(&old.section.blocks[0]).unwrap()),
            "The interface continues on the next page."
        );
        assert_eq!(old.section.blocks.len(), 2);
        assert_eq!(next.section.blocks.len(), 1);
        assert_eq!(count, old.provenance.len() + next.provenance.len());
        assert!(
            old.provenance
                .iter()
                .all(|p| p.source.start.spine == old.section.id)
        );
        assert_eq!(old.section.anchors[0].source.spine, old.section.id);
        assert!(
            paragraph(&old.section.blocks[0])
                .unwrap()
                .content
                .iter()
                .any(|i| matches!(i, Inline::Text(r) if r.link.as_ref() == Some(&link)))
        );
    }
}
