//! Conversion-only grouping. Keep the PDF's coordinates and render one crop;
//! never concatenate embedded rasters or do this work on reader frames.
use super::*;

const MAX_REGIONS: usize = 256;
const MAX_ENCLOSURES: usize = 128;
const MAX_LINES: usize = 4096;
const MAX_MEMBERS: usize = 32;

fn combine(regions: &mut Vec<Region>, mut members: Vec<usize>, bounds: Rect) {
    members.sort_unstable();
    let first = members[0];
    let mut combined = Region::graphic(bounds);
    for index in members.into_iter().rev() {
        combined.merge(regions.remove(index));
    }
    regions.insert(first, combined);
}

fn contains(outer: Rect, inner: Rect) -> bool {
    outer.inflate(0.5, 0.5).contains_rect(inner)
}

fn blocked_by_object(bounds: Rect, objects: &[(Rect, Block)]) -> bool {
    objects
        .iter()
        .any(|(rect, _)| bounds.intersect(*rect).area() > 0.0)
}

/// A real frame/background is stronger than a shared caption. Its interior
/// text belongs to the artwork, but tables and independently captioned figures
/// remain semantic boundaries. Process nested enclosures from the inside out.
fn enclosed(
    regions: &mut Vec<Region>,
    prose: &[Line],
    page: &NativePage,
    objects: &[(Rect, Block)],
    body: f64,
) {
    let mut enclosures = page
        .enclosures
        .iter()
        .map(|r| Rect::new(r[0], r[1], r[2], r[3]))
        .collect::<Vec<_>>();
    enclosures.sort_by(|a, b| a.area().total_cmp(&b.area()));
    for bounds in enclosures {
        if bounds.width() < body * 4.0
            || bounds.height() < body * 4.0
            || bounds.area() >= page.width * page.height * 0.8
            || blocked_by_object(bounds, objects)
        {
            continue;
        }
        let members = regions
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.recovery && contains(bounds, r.bounds))
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if !(2..=MAX_MEMBERS).contains(&members.len()) {
            continue;
        }
        let union = members
            .iter()
            .map(|&i| regions[i].bounds)
            .reduce(|a, b| a.union(b))
            .unwrap();
        if union.width() < bounds.width() * 0.5
            || union.height() < bounds.height() * 0.5
            || bounds.area() > union.area() * 2.5
            || regions
                .iter()
                .enumerate()
                .any(|(i, r)| !members.contains(&i) && bounds.intersect(r.bounds).area() > 0.0)
            || prose.iter().any(|line| {
                bounds.contains(line.rect.center())
                    && (caption_prefix_len(&line.text()).is_some()
                        || line.glyphs.iter().any(|g| {
                            g.tag.as_deref().is_some_and(|t| {
                                t.strip_prefix('H').is_some_and(|n| {
                                    n.parse::<u8>().is_ok_and(|level| (1..=6).contains(&level))
                                })
                            })
                        }))
                    && !members
                        .iter()
                        .any(|&i| contains(regions[i].bounds, line.rect))
            })
        {
            continue;
        }
        combine(regions, members, bounds);
    }
}

fn raster_panel(region: &Region, body: f64) -> bool {
    !region.recovery
        && !region.image_sources.is_empty()
        && region.bounds.width() >= body * 2.0
        && region.bounds.height() >= body * 2.0
}

fn same_row(a: Rect, b: Rect, body: f64) -> bool {
    let overlap = a.y1.min(b.y1) - a.y0.max(b.y0);
    let gap = b.x0 - a.x1;
    let tolerance = (body * 2.0)
        .max(a.width().min(b.width()) * 0.12)
        .min(body * 4.0);
    gap >= -0.5
        && gap <= tolerance
        && overlap >= a.height().max(b.height()) * 0.8
        && ((a.y0 - b.y0).abs() <= body || (a.y1 - b.y1).abs() <= body)
}

/// Reject prose in the prospective union and separate text/captions just below
/// each panel. A single line spanning the pair may be their shared caption.
fn row_clear(
    bounds: Rect,
    members: &[usize],
    regions: &[Region],
    prose: &[Line],
    body: f64,
) -> bool {
    let nearby = prose
        .iter()
        .filter(|l| {
            l.rect.y1 >= bounds.y0
                && l.rect.y0 <= bounds.y1 + body * 2.5
                && l.rect.x1 > bounds.x0
                && l.rect.x0 < bounds.x1
        })
        .collect::<Vec<_>>();
    if nearby.iter().any(|line| {
        line.glyphs.iter().any(|g| {
            !g.text.chars().all(char::is_whitespace)
                && bounds.contains(g.bounds().center())
                && !members
                    .iter()
                    .any(|&i| regions[i].bounds.contains(g.bounds().center()))
        })
    }) {
        return false;
    }
    let leading = nearby
        .iter()
        .filter(|l| l.rect.y0 >= bounds.y1 - body * 0.2)
        .collect::<Vec<_>>();
    // Separate captions/prose columns indicate ownership of distinct pictures,
    // including staggered descriptions with no figure number.
    let owners = members
        .iter()
        .filter(|&&i| {
            leading
                .iter()
                .any(|l| contains(regions[i].bounds.inflate(body, body * 3.0), l.rect))
        })
        .count();
    owners < 2
        && leading
            .iter()
            .filter(|l| caption_prefix_len(&l.text()).is_some())
            .count()
            < 2
}

fn rows(regions: &mut Vec<Region>, prose: &[Line], objects: &[(Rect, Block)], body: f64) {
    let mut candidates = regions
        .iter()
        .enumerate()
        .filter(|(_, r)| raster_panel(r, body))
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    candidates.sort_by(|&a, &b| {
        regions[a]
            .bounds
            .x0
            .total_cmp(&regions[b].bounds.x0)
            .then(regions[a].bounds.y0.total_cmp(&regions[b].bounds.y0))
    });
    let mut assigned = vec![false; regions.len()];
    let mut groups = Vec::new();
    let glyphs = prose.iter().map(|l| l.glyphs.len()).sum::<usize>().max(1);
    let mut text_checks = 2_000_000 / glyphs;
    for &seed in &candidates {
        if assigned[seed] {
            continue;
        }
        let reference = regions[seed].bounds;
        let mut members = vec![seed];
        let mut bounds = reference;
        for &next in &candidates {
            if next == seed || assigned[next] || members.len() == MAX_MEMBERS {
                continue;
            }
            let b = regions[next].bounds;
            if b.x0 > bounds.x1 + body * 4.0 {
                break;
            }
            // Compare every added panel to the original row as well as the
            // right edge. This prevents a staircase of neighbors growing down
            // the page through transitive near-alignment.
            if !same_row(bounds, b, body)
                || (reference.y0 - b.y0).abs() > body
                || (reference.y1 - b.y1).abs() > body
            {
                continue;
            }
            let union = bounds.union(b);
            let mut proposed = members.clone();
            proposed.push(next);
            if text_checks == 0 {
                break;
            }
            text_checks -= 1;
            if blocked_by_object(union, objects)
                || regions
                    .iter()
                    .enumerate()
                    .any(|(i, r)| !proposed.contains(&i) && union.intersect(r.bounds).area() > 0.0)
                || !row_clear(union, &proposed, regions, prose, body)
            {
                continue;
            }
            members = proposed;
            bounds = union;
        }
        if members.len() > 1 {
            for &member in &members {
                assigned[member] = true;
            }
            groups.push((members, bounds));
        }
    }
    // Original PDF paint order can interleave groups. Empty stable slots only
    // after all decisions, so removing one group never shifts another's IDs.
    let mut slots = std::mem::take(regions)
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>();
    for (members, bounds) in groups {
        let first = *members.iter().min().unwrap();
        let mut combined = Region::graphic(bounds);
        for member in members {
            combined.merge(slots[member].take().unwrap());
        }
        slots[first] = Some(combined);
    }
    *regions = slots.into_iter().flatten().collect();
}

pub(super) fn group(
    regions: &mut Vec<Region>,
    prose: &[Line],
    page: &NativePage,
    objects: &[(Rect, Block)],
    body: f64,
) {
    // Bounded page-local work. Dense diagrams retain the established overlap
    // and numbered-subfigure path rather than paying quadratic text scans.
    if regions.len() > MAX_REGIONS
        || page.enclosures.len() > MAX_ENCLOSURES
        || prose.len() > MAX_LINES
        || prose.iter().map(|l| l.glyphs.len()).sum::<usize>() > 20_000
        || !body.is_finite()
        || body <= 0.0
    {
        return;
    }
    enclosed(regions, prose, page, objects, body);
    rows(regions, prose, objects, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(bounds: [f64; 4], id: usize) -> Region {
        Region {
            bounds: Rect::new(bounds[0], bounds[1], bounds[2], bounds[3]),
            image_sources: vec![id],
            recovery: false,
        }
    }

    fn row(page: &mut NativePage, text: &str, x: f64, y: f64, size: f64) {
        for (offset, c) in text.chars().enumerate() {
            let x = x + offset as f64 * size * 0.5;
            page.glyphs.push(NativeGlyph {
                text: c.to_string(),
                rect: [x, y - size, x + size * 0.5, y],
                baseline: [x, y],
                advance: [x + size * 0.5, y],
                size,
                bold: false,
                italic: false,
                rotated: false,
                tag: None,
                mcid: None,
                link: None,
                index: page.glyphs.len(),
                unmapped: false,
            });
        }
    }

    fn page() -> NativePage {
        NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        }
    }

    #[test]
    fn unnumbered_pair_keeps_positions_caption_and_single_glyph_ownership() {
        let mut page = page();
        // PDF paint order and slight top offsets must not reorder the panels.
        page.images = vec![[262.0, 99.2, 462.0, 249.2], [50.0, 100.0, 250.0, 250.0]];
        row(&mut page, "A shared description", 60.0, 270.0, 8.0);
        row(&mut page, "continues here.", 60.0, 280.0, 8.0);
        row(&mut page, "Body resumes separately.", 50.0, 305.0, 10.0);
        let (stored, crops) =
            build(&page, 0, 10.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert_eq!(crops.len(), 1);
        let mut ids = crops[0].image_sources.clone();
        ids.sort_unstable();
        assert_eq!(ids, [0, 1]);
        assert_eq!(crops[0].bounds, Rect::new(48.0, 97.2, 464.0, 252.0));
        let Block::Figure(figure) = &stored.section.blocks[0] else {
            panic!("shared caption is outside bitmap");
        };
        assert_eq!(figure.images.len(), 1);
        assert_eq!(figure.captions.len(), 1);
        let caption = figure.captions[0]
            .content
            .iter()
            .filter_map(|i| {
                if let Inline::Text(t) = i {
                    Some(t.text.as_str())
                } else {
                    None
                }
            })
            .collect::<String>();
        assert_eq!(caption, "A shared description continues here.");
        assert!(
            matches!(&stored.section.blocks[1], Block::Text(t) if t.kind == TextBlockKind::Paragraph)
        );
        assert_eq!(stored.provenance.len(), page.glyphs.len());
        assert_eq!(
            stored
                .provenance
                .iter()
                .map(|p| p.original_glyph)
                .collect::<HashSet<_>>()
                .len(),
            page.glyphs.len()
        );
    }

    #[test]
    fn shared_frame_owns_banner_vectors_and_internal_text_but_not_next_picture() {
        let mut page = page();
        page.images = vec![
            [60.0, 100.0, 200.0, 350.0],
            [212.0, 100.5, 352.0, 350.5],
            [60.0, 365.0, 352.0, 425.0],
            [60.0, 550.0, 200.0, 680.0],
        ];
        page.enclosures = vec![[50.0, 90.0, 362.0, 460.0]];
        page.graphics = vec![[64.0, 432.0, 80.0, 447.0]];
        row(&mut page, "Inside artwork", 95.0, 449.0, 8.0);
        row(&mut page, "Poster description.", 65.0, 480.0, 8.0);
        let (stored, crops) =
            build(&page, 0, 10.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert_eq!(crops.len(), 2, "{crops:?}");
        assert_eq!(crops[0].bounds, Rect::new(48.0, 88.0, 364.0, 462.0));
        let mut ids = crops[0].image_sources.clone();
        ids.sort_unstable();
        assert_eq!(ids, [0, 1, 2]);
        assert_eq!(crops[1].image_sources, [3]);
        assert_eq!(stored.provenance.len(), page.glyphs.len());
        for glyph in &page.glyphs[.."Inside artwork".len()] {
            assert!(
                stored
                    .provenance
                    .iter()
                    .any(|p| p.original_glyph == glyph.index
                        && p.source.start.node.contains("image"))
            );
        }
    }

    #[test]
    fn row_group_rejects_prose_separate_captions_recovery_and_other_regions() {
        for obstacle in [
            "prose",
            "captions",
            "unnumbered",
            "staggered",
            "region",
            "recovery",
        ] {
            let mut page = page();
            let mut regions = vec![
                panel([50.0, 100.0, 200.0, 250.0], 0),
                panel([220.0, 100.0, 370.0, 250.0], 1),
            ];
            match obstacle {
                "prose" => row(&mut page, "X", 204.0, 180.0, 10.0),
                "captions" | "unnumbered" | "staggered" => {
                    let (a, b) = if obstacle == "captions" {
                        ("Figure 1 Left", "Figure 2 Right")
                    } else {
                        ("Left description", "Right description")
                    };
                    row(&mut page, a, 60.0, 270.0, 8.0);
                    row(
                        &mut page,
                        b,
                        230.0,
                        if obstacle == "staggered" {
                            280.0
                        } else {
                            270.0
                        },
                        8.0,
                    );
                }
                "region" => regions.push(Region::graphic(Rect::new(202.0, 140.0, 215.0, 160.0))),
                "recovery" => regions[1].recovery = true,
                _ => unreachable!(),
            }
            let count = regions.len();
            group(&mut regions, &lines(page.glyphs.clone()), &page, &[], 10.0);
            assert_eq!(regions.len(), count, "{obstacle}");
        }
    }

    #[test]
    fn groups_survive_interleaved_paint_order_without_crossing_rows() {
        let mut regions = vec![
            panel([50.0, 100.0, 150.0, 200.0], 0),
            panel([50.0, 300.0, 150.0, 400.0], 1),
            panel([162.0, 100.0, 262.0, 200.0], 2),
            panel([162.0, 300.0, 262.0, 400.0], 3),
        ];
        group(&mut regions, &[], &page(), &[], 10.0);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].image_sources, [0, 2]);
        assert_eq!(regions[1].image_sources, [1, 3]);
    }

    #[test]
    fn unframed_staircase_and_banner_do_not_chain_into_a_page() {
        let mut regions = vec![
            panel([50.0, 100.0, 150.0, 200.0], 0),
            panel([162.0, 106.0, 262.0, 206.0], 1),
            panel([274.0, 112.0, 374.0, 212.0], 2),
            panel([50.0, 225.0, 374.0, 300.0], 3),
        ];
        group(&mut regions, &[], &page(), &[], 10.0);
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0].image_sources, [0, 1]);
        assert_eq!(regions[1].image_sources, [2]);
        assert_eq!(regions[2].image_sources, [3]);
    }

    #[test]
    fn enclosing_page_background_and_captioned_pictures_stay_separate() {
        let mut page = page();
        // Extraction removes full-page fills; oversize enclosures are also
        // rejected here by their very low component coverage.
        page.enclosures = vec![[0.0, 0.0, 600.0, 800.0], [40.0, 90.0, 380.0, 280.0]];
        row(&mut page, "Figure 1 Left", 60.0, 270.0, 8.0);
        row(&mut page, "Figure 2 Right", 230.0, 270.0, 8.0);
        let mut regions = vec![
            panel([50.0, 100.0, 200.0, 250.0], 0),
            panel([220.0, 100.0, 370.0, 250.0], 1),
        ];
        group(&mut regions, &lines(page.glyphs.clone()), &page, &[], 10.0);
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn completed_table_blocks_are_barriers_even_inside_common_frame() {
        let mut page = page();
        page.enclosures = vec![[40.0, 90.0, 380.0, 260.0]];
        let mut regions = vec![
            panel([50.0, 100.0, 200.0, 250.0], 0),
            panel([220.0, 100.0, 370.0, 250.0], 1),
        ];
        let objects = [(
            Rect::new(203.0, 150.0, 217.0, 180.0),
            Block::Table(TableBlock { visualization: None,
                before: vec![],
                after: vec![],
                rows: vec![],
                source: None,
            }),
        )];
        group(&mut regions, &[], &page, &objects, 10.0);
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn dense_pages_have_bounded_grouping_work() {
        let mut regions = (0..=MAX_REGIONS)
            .map(|i| {
                panel(
                    [50.0 + i as f64 * 25.0, 100.0, 70.0 + i as f64 * 25.0, 200.0],
                    i,
                )
            })
            .collect::<Vec<_>>();
        group(&mut regions, &[], &page(), &[], 10.0);
        assert_eq!(regions.len(), MAX_REGIONS + 1);
    }
}
