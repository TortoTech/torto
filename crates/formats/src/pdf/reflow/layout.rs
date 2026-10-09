use super::*;
use std::collections::HashSet;
mod chrome;
mod figures;
mod headings;
pub(super) use chrome::Detector as ChromeDetector;

#[derive(Clone)]
struct Line {
    glyphs: Vec<NativeGlyph>,
    rect: Rect,
    baseline: f64,
    size: f64,
}

impl Line {
    fn text(&self) -> String {
        self.glyphs
            .iter()
            .map(|g| g.text.as_str())
            .collect::<String>()
    }
    fn heading(&self, body: f64) -> Option<u8> {
        if let Some(level) = self
            .glyphs
            .iter()
            .filter_map(|g| g.tag.as_deref())
            .find_map(|tag| {
                tag.strip_prefix('H')
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|n| (1..=6).contains(n))
            })
        {
            return Some(level);
        }
        // Bold alone is not evidence of a heading: run-in headings retain their
        // paragraph. Large type must also occupy a short isolated line.
        (self.size > body * 1.2 && self.text().chars().count() < 160)
            .then_some(if self.size > body * 1.6 { 1 } else { 2 })
    }

    fn prose_bounds(&self) -> Rect {
        self.glyphs
            .iter()
            .filter(|g| {
                g.size >= self.size * 0.8
                    && g.size <= self.size * 1.25
                    && !g.text.chars().all(char::is_whitespace)
            })
            .map(NativeGlyph::bounds)
            .reduce(|a, b| a.union(b))
            .unwrap_or(self.rect)
    }
}

fn lines(glyphs: impl IntoIterator<Item = NativeGlyph>) -> Vec<Line> {
    let mut glyphs = glyphs.into_iter().collect::<Vec<_>>();
    glyphs.sort_by(|a, b| {
        a.baseline[1]
            .total_cmp(&b.baseline[1])
            .then(a.rect[0].total_cmp(&b.rect[0]))
    });
    let mut bands: Vec<Line> = Vec::new();
    for glyph in glyphs {
        // Keep each band's initial baseline fixed. Neither a large ordinal nor
        // a sequence of slightly offset glyphs may expand it into the next row.
        // Small raised/lowered runs are attached separately after column gaps
        // have been split, so they do not set the tolerance for ordinary text.
        let band = bands
            .iter_mut()
            .rev()
            .take_while(|line| glyph.baseline[1] - line.baseline <= glyph.size * 0.25)
            .find(|line| {
                let overlap = line.rect.y1.min(glyph.rect[3]) - line.rect.y0.max(glyph.rect[1]);
                glyph.rotated == line.glyphs[0].rotated
                    && (glyph.baseline[1] - line.baseline).abs() <= line.size.min(glyph.size) * 0.25
                    && overlap > line.rect.height().min(glyph.bounds().height()) * 0.15
            });
        if let Some(line) = band {
            line.rect = line.rect.union(glyph.bounds());
            line.size = line.size.min(glyph.size);
            line.glyphs.push(glyph);
        } else {
            bands.push(Line {
                rect: glyph.bounds(),
                baseline: glyph.baseline[1],
                size: glyph.size,
                glyphs: vec![glyph],
            });
        }
    }
    let mut result = Vec::new();
    for mut band in bands {
        let size = typical_metric(&band.glyphs, |g| g.size);
        band.glyphs
            .sort_by(|a, b| a.rect[0].total_cmp(&b.rect[0]).then(a.index.cmp(&b.index)));
        let mut segment: Vec<NativeGlyph> = Vec::new();
        for glyph in band.glyphs {
            if segment
                .last()
                .is_some_and(|old| glyph.rect[0] - old.advance[0] > size * 3.0)
            {
                result.push(make_line(std::mem::take(&mut segment)));
            }
            segment.push(glyph);
        }
        if !segment.is_empty() {
            result.push(make_line(segment));
        }
    }
    attach_scripts(result)
}

/// Character-weighted median: a ligature/text run counts as its visible text,
/// while overlapping PDF spaces cannot determine the prose font or baseline.
fn typical_metric(glyphs: &[NativeGlyph], metric: impl Fn(&NativeGlyph) -> f64) -> f64 {
    let mut values = glyphs
        .iter()
        .filter_map(|g| {
            let weight = g.text.chars().filter(|c| !c.is_whitespace()).count();
            (weight > 0).then(|| (metric(g), weight))
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        return metric(&glyphs[0]);
    }
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    let middle = values.iter().map(|(_, weight)| weight).sum::<usize>() / 2;
    let mut count = 0;
    for (value, weight) in values {
        count += weight;
        if count > middle {
            return value;
        }
    }
    unreachable!()
}

fn script_distance(script: &Line, parent: &Line) -> Option<f64> {
    let delta = (script.baseline - parent.baseline).abs();
    if !script_size(script.size, parent.size)
        || delta < parent.size * 0.12
        || delta > parent.size * 0.8
        || script.glyphs[0].rotated != parent.glyphs[0].rotated
    {
        return None;
    }
    let mut nearest = f64::INFINITY;
    let mut overlap = 0.0;
    let mut width = 0.0;
    let mut vertical_overlap = false;
    for small in &script.glyphs {
        if small.text.chars().all(char::is_whitespace) {
            continue;
        }
        let mut collision: f64 = 0.0;
        width += small.bounds().width();
        for normal in parent.glyphs.iter().filter(|g| {
            g.size >= parent.size * 0.8
                && g.size <= parent.size * 1.25
                && !g.text.chars().all(char::is_whitespace)
        }) {
            let x_overlap = small.rect[2].min(normal.rect[2]) - small.rect[0].max(normal.rect[0]);
            nearest = nearest.min((-x_overlap).max(0.0));
            collision = collision.max(x_overlap.max(0.0));
            let y_overlap = small.rect[3].min(normal.rect[3]) - small.rect[1].max(normal.rect[1]);
            vertical_overlap |=
                y_overlap > small.bounds().height().min(normal.bounds().height()) * 0.15;
        }
        overlap += collision;
    }
    // A separate small-print row overlaps body glyphs horizontally; a script
    // sits beside them. Do not attach another row just because its box is tall.
    (vertical_overlap && nearest <= parent.size * 0.8 && overlap <= width * 0.35)
        .then_some(delta / parent.size + nearest / parent.size)
}

fn script_size(size: f64, parent: f64) -> bool {
    // Font dictionaries round sizes independently (7.9701 vs 9.9626 * 0.8).
    // Baseline and collision checks still decide whether a small run is a script.
    size <= parent * (0.8 + 0.0001)
}

fn attach_scripts(mut lines: Vec<Line>) -> Vec<Line> {
    lines.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
    let radius = lines.iter().map(|l| l.size).fold(0.0, f64::max) * 0.8;
    let parents = lines
        .iter()
        .map(|script| {
            let start = lines.partition_point(|l| l.baseline < script.baseline - radius);
            let end = lines.partition_point(|l| l.baseline <= script.baseline + radius);
            (start..end)
                .filter_map(|index| {
                    script_distance(script, &lines[index]).map(|score| (index, score))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(index, _)| index)
        })
        .collect::<Vec<_>>();
    let mut grouped = vec![Vec::new(); lines.len()];
    for (index, line) in lines.into_iter().enumerate() {
        let mut root = index;
        // A parent is strictly larger, so these links cannot cycle.
        while let Some(parent) = parents[root] {
            root = parent;
        }
        grouped[root].extend(line.glyphs);
    }
    grouped
        .into_iter()
        .filter(|glyphs| !glyphs.is_empty())
        .map(|mut glyphs| {
            glyphs.sort_by(|a, b| a.rect[0].total_cmp(&b.rect[0]).then(a.index.cmp(&b.index)));
            make_line(glyphs)
        })
        .collect()
}

fn make_line(glyphs: Vec<NativeGlyph>) -> Line {
    let rect = glyphs
        .iter()
        .map(NativeGlyph::bounds)
        .reduce(|a, b| a.union(b))
        .unwrap();
    let size = typical_metric(&glyphs, |g| g.size);
    let baseline = typical_metric(&glyphs, |g| g.baseline[1]);
    Line {
        glyphs,
        rect,
        size,
        baseline,
    }
}

fn edge_key(line: &Line, height: f64) -> Option<String> {
    let edge = if line.baseline < height * 0.18 {
        "top"
    } else if line.baseline > height * 0.82 {
        "bottom"
    } else {
        return None;
    };
    let raw = line.text();
    let counter = raw
        .split_whitespace()
        .next()
        .into_iter()
        .chain(raw.split_whitespace().last())
        .any(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()));
    let mut text = String::new();
    let mut digit = false;
    for c in line.text().chars().filter(|c| !c.is_whitespace()) {
        if c.is_ascii_digit() {
            if !digit {
                text.push('#');
            }
            digit = true;
        } else {
            text.extend(c.to_lowercase());
            digit = false;
        }
    }
    (!text.is_empty()).then(|| {
        format!(
            "{edge}:{}:{}:{}:{text}",
            (line.baseline / height * 100.0).round(),
            (line.size * 2.0).round(),
            if counter { "counter" } else { "plain" },
        )
    })
}

fn edge_lines(page: &NativePage, body: Option<f64>) -> Vec<Line> {
    let grouped = lines(page.glyphs.clone());
    grouped
        .iter()
        .enumerate()
        .filter(|(index, line)| {
            !line.glyphs[0].rotated
                && body.is_none_or(|body| line.size <= body * 1.15)
                // Diagram labels can dominate a page's font distribution. Font
                // size alone must not veto candidates before repetition/layout
                // proof. Protect actual tagged and outline-validated headings.
                && !line.glyphs.iter().any(|g| {
                    g.tag.as_deref().is_some_and(|tag| {
                        tag.strip_prefix('H')
                            .and_then(|n| n.parse::<u8>().ok())
                            .is_some_and(|n| (1..=6).contains(&n))
                    }) || page.headings.iter().any(|h| {
                        h.ordinal.contains(&g.index) || h.title.contains(&g.index)
                    })
                })
                && line.text().chars().count() <= 160
                && edge_key(line, page.height).is_some()
                && if line.baseline < page.height * 0.18 {
                    grouped
                        .get(index + 1)
                        .is_some_and(|next| next.baseline - line.baseline >= line.size * 2.0)
                } else {
                    index
                        .checked_sub(1)
                        .and_then(|i| grouped.get(i))
                        .is_some_and(|previous| {
                            line.baseline - previous.baseline >= line.size * 2.0
                        })
                }
        })
        .map(|(_, line)| line.clone())
        .collect()
}

#[cfg(test)]
pub(super) fn edge_signatures(page: &NativePage) -> HashSet<String> {
    edge_lines(page, Some(typical_metric(&page.glyphs, |g| g.size)))
        .iter()
        .filter_map(|line| edge_key(line, page.height))
        .collect()
}

pub(super) fn repeats_near_page(pages: &[usize], index: usize, key: &str) -> bool {
    // A running chapter title need not occur in 20% of a long book. Compare
    // nearby pages, including alternating recto/verso headers, in a fixed window.
    let start = pages.partition_point(|p| *p < index.saturating_sub(10));
    let end = pages.partition_point(|p| *p <= index.saturating_add(10));
    // A separated marginal line with an outer page number is stronger evidence,
    // and short sections can have only two matching verso headers.
    end - start >= if key.contains(":counter:") { 2 } else { 3 }
}

pub(super) fn mark_outline_headings(page: &mut NativePage, labels: &[(String, u8)]) {
    headings::mark(page, labels);
}

/// Image identity survives geometric grouping; recovery crops stay barriers.
#[derive(Debug)]
pub(super) struct Crop {
    pub path: String,
    pub bounds: Rect,
    pub image_sources: Vec<usize>,
}

struct Region {
    bounds: Rect,
    image_sources: Vec<usize>,
    recovery: bool,
}

impl Region {
    fn graphic(bounds: Rect) -> Self {
        Self {
            bounds,
            image_sources: Vec::new(),
            recovery: false,
        }
    }

    fn merge(&mut self, other: Self) {
        self.bounds = self.bounds.union(other.bounds);
        self.image_sources.extend(other.image_sources);
        self.recovery |= other.recovery;
    }
}

struct Builder {
    section: Section,
    provenance: Vec<SourceSlice>,
    crops: Vec<Crop>,
    page: usize,
    body: f64,
    width: f64,
    height: f64,
}

impl Builder {
    fn range(&self, node: String, start: u64, end: u64) -> SourceRange {
        SourceRange {
            start: SourceAnchor {
                spine: self.section.id.clone(),
                node: node.clone(),
                text_offset: start,
            },
            end: SourceAnchor {
                spine: self.section.id.clone(),
                node,
                text_offset: end,
            },
        }
    }

    fn text(&mut self, lines: &[Line], kind: TextBlockKind) -> TextBlock {
        let node = format!("p{}-g{}", self.page + 1, lines[0].glyphs[0].index);
        let mut runs: Vec<TextRun> = Vec::new();
        let mut offset = 0_u64;
        let caption_words = (kind == TextBlockKind::Caption).then(|| {
            lines
                .iter()
                .map(Line::text)
                .collect::<Vec<_>>()
                .join("\n")
                .split(|c: char| !c.is_alphabetic())
                .filter(|word| !word.is_empty())
                .map(str::to_lowercase)
                .collect::<HashSet<_>>()
        });
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                let previous = &lines[index - 1];
                let old = previous.glyphs.last().and_then(|g| g.text.chars().last());
                let next = line.glyphs[0].text.chars().next();
                // Preserve ASCII compounds unless a confirmed caption supplies
                // an independent complete spelling of the split word.
                if old == Some('\u{ad}') {
                    if let Some(last) = runs.last_mut() {
                        last.text.pop();
                        offset = offset.saturating_sub(1);
                    }
                    if let Some(last) = self.provenance.last_mut() {
                        last.source.end.text_offset = last.source.start.text_offset;
                    }
                } else if old == Some('-')
                    && caption_words
                        .as_ref()
                        .is_some_and(|words| caption_hyphen_join(&runs, line, words))
                {
                    // A complete spelling elsewhere in this caption proves this
                    // is a line-end split, rather than an authored compound.
                    runs.last_mut().unwrap().text.pop();
                    offset = offset.saturating_sub(1);
                    if let Some(last) = self.provenance.last_mut() {
                        last.source.end.text_offset = last
                            .source
                            .end
                            .text_offset
                            .saturating_sub(1)
                            .max(last.source.start.text_offset);
                    }
                } else if old.is_some_and(|c| !c.is_whitespace() && c != '-' && !cjk(c))
                    && next.is_some_and(|c| !c.is_whitespace() && !cjk(c))
                {
                    if let Some(last) = runs.last_mut() {
                        last.text.push(' ');
                        offset += 1;
                    }
                }
            }
            let mut previous: Option<&NativeGlyph> = None;
            for glyph in &line.glyphs {
                let style = TextStyle {
                    bold: glyph.bold,
                    italic: glyph.italic,
                    link_role: if glyph
                        .link
                        .as_deref()
                        .is_some_and(|s| s.contains("#pdf-native-note-"))
                    {
                        LinkRole::FootnoteReference
                    } else {
                        LinkRole::Normal
                    },
                    size_scale: if kind.is_heading() {
                        1.0
                    } else {
                        (glyph.size / self.body).clamp(0.7, 1.4) as f32
                    },
                    baseline: if script_size(glyph.size, line.size)
                        && glyph.baseline[1] < line.baseline - line.size * 0.12
                    {
                        TextBaseline::Superscript
                    } else if script_size(glyph.size, line.size)
                        && glyph.baseline[1] > line.baseline + line.size * 0.12
                    {
                        TextBaseline::Subscript
                    } else {
                        TextBaseline::Normal
                    },
                    ..TextStyle::default()
                };
                if let Some(old) = previous {
                    let gap = glyph.baseline[0] - old.advance[0];
                    if gap > line.size * 0.15
                        && !old.text.ends_with(char::is_whitespace)
                        && !glyph.text.starts_with(char::is_whitespace)
                        && !old.text.chars().last().is_some_and(cjk)
                        && !glyph.text.chars().next().is_some_and(cjk)
                    {
                        if let Some(last) = runs.last_mut() {
                            last.text.push(' ');
                            offset += 1;
                        }
                    }
                }
                let link = glyph.link.as_deref().and_then(|s| {
                    PublicationUrl::website(s).or_else(|| PublicationUrl::parse(s).ok())
                });
                let start = offset;
                offset += glyph.text.chars().count() as u64;
                self.provenance.push(SourceSlice {
                    source: self.range(node.clone(), start, offset),
                    page: self.page + 1,
                    rect: glyph.rect,
                    original_glyph: glyph.index,
                });
                if let Some(last) = runs
                    .last_mut()
                    .filter(|run| run.style == style && run.link == link)
                {
                    last.text.push_str(&glyph.text);
                } else {
                    runs.push(TextRun {
                        text: glyph.text.clone(),
                        style,
                        link,
                    });
                }
                previous = Some(glyph);
            }
        }
        TextBlock {
            kind,
            content: runs.into_iter().map(Inline::Text).collect(),
            style: BlockStyle::default(),
            source: Some(self.range(node, 0, offset)),
        }
    }

    fn image(&mut self, rect: Rect, glyphs: &[NativeGlyph], formula: bool) -> Block {
        let rect = rect.intersect(Rect::new(0.0, 0.0, self.width, self.height));
        let path = format!(
            "resources/page-{}-region-{}.png",
            self.page + 1,
            self.crops.len()
        );
        let source = self.range(
            format!("p{}-image{}", self.page + 1, self.crops.len()),
            0,
            0,
        );
        for glyph in glyphs {
            self.provenance.push(SourceSlice {
                source: source.clone(),
                page: self.page + 1,
                rect: glyph.rect,
                original_glyph: glyph.index,
            });
        }
        self.crops.push(Crop {
            path: path.clone(),
            bounds: rect,
            image_sources: Vec::new(),
        });
        Block::Image(ImageBlock {
            formula_image: formula,
            formula: None,
            href: PublicationUrl::parse(&path).unwrap(),
            alt: if formula {
                "PDF formula".into()
            } else {
                "PDF original region".into()
            },
            style: ImageStyle::default(),
            source: Some(source),
            text_layer: None,
        })
    }
}

pub(super) fn build(
    page: &NativePage,
    index: usize,
    body: f64,
    repeated: &HashSet<String>,
    stats: &mut Statistics,
) -> Result<(StoredSection, Vec<Crop>), String> {
    let mut builder = Builder {
        section: Section {
            id: spine_id(index)?,
            href: section_href(index)?,
            blocks: Vec::new(),
            anchors: Vec::new(),
        },
        provenance: Vec::new(),
        crops: Vec::new(),
        page: index,
        body,
        width: page.width,
        height: page.height,
    };
    let mut removed = 0;
    let fallback = requires_page_fallback(page);
    if fallback {
        let block = builder.image(
            Rect::new(0.0, 0.0, page.width, page.height),
            &page.glyphs,
            false,
        );
        builder.section.blocks.push(block);
        stats.fallback_pages += 1;
    } else {
        stats.text_pages += 1;
        let mut remaining = vec![true; page.glyphs.len()];
        for line in edge_lines(page, Some(body)) {
            if edge_key(&line, page.height).is_some_and(|key| repeated.contains(&key)) {
                for glyph in line.glyphs {
                    remaining[glyph.index] = false;
                    removed += 1;
                    stats.removed_chrome_glyphs += 1;
                }
            }
        }
        let mut objects: Vec<(Rect, Block)> = Vec::new();
        // Tables precede column splitting. Cells own their original glyphs once.
        for (rect, xs, ys) in grids(page).into_iter().chain(borderless_grids(page, body)) {
            let provenance_start = builder.provenance.len();
            let glyphs: Vec<_> = page
                .glyphs
                .iter()
                .filter(|g| remaining[g.index] && rect.contains(g.bounds().center()))
                .cloned()
                .collect();
            if glyphs.is_empty() {
                continue;
            }
            if glyphs.iter().any(|g| g.unmapped) {
                for glyph in &glyphs {
                    remaining[glyph.index] = false;
                }
                let block = builder.image(rect, &glyphs, false);
                objects.push((rect, block));
                stats.fallback_regions += 1;
                continue;
            }
            let mut rows = Vec::new();
            let mut filled = 0;
            for (row_index, y) in ys.windows(2).enumerate() {
                let mut cells = Vec::new();
                for x in xs.windows(2) {
                    let bounds = Rect::new(x[0], y[0], x[1], y[1]);
                    let cell_lines = lines(
                        glyphs
                            .iter()
                            .filter(|g| bounds.contains(g.bounds().center()))
                            .cloned(),
                    );
                    let text = if cell_lines.is_empty() {
                        TextBlock {
                            kind: TextBlockKind::Paragraph,
                            content: Vec::new(),
                            style: BlockStyle::default(),
                            source: None,
                        }
                    } else {
                        filled += 1;
                        builder.text(&cell_lines, TextBlockKind::Paragraph)
                    };
                    cells.push(TableCell {
                        text,
                        authored_alignment: None,
                        column_span: 1,
                        row_span: 1,
                        header: row_index == 0,
                    });
                }
                rows.push(TableRow { cells });
            }
            // A frame or decorative divider is not a data table.
            if filled < 3 {
                builder.provenance.truncate(provenance_start);
                continue;
            }
            for glyph in &glyphs {
                remaining[glyph.index] = false;
            }
            stats.tables += 1;
            objects.push((
                rect,
                Block::Table(TableBlock {
                    before: Vec::new(),
                    after: Vec::new(),
                    rows,
                    source: Some(builder.range(
                        format!("p{}-table{}", index + 1, objects.len()),
                        0,
                        0,
                    )),
                }),
            ));
        }
        let mut regions = page
            .images
            .iter()
            .enumerate()
            .map(|(index, r)| Region {
                bounds: Rect::new(r[0], r[1], r[2], r[3]),
                image_sources: vec![index],
                recovery: false,
            })
            .chain(
                page.graphics
                    .iter()
                    .map(|r| Region::graphic(Rect::new(r[0], r[1], r[2], r[3]))),
            )
            .filter(|region| {
                let rect = region.bounds;
                rect.width() > body
                    && rect.height() > body
                    && rect.area() < page.width * page.height * 0.85
                    && !objects
                        .iter()
                        .any(|(table, _)| table.intersect(rect).area() > rect.area() * 0.5)
            })
            .collect::<Vec<_>>();
        // Incomplete grids (merged cells or irregular ruling) keep their exact
        // appearance rather than silently flattening a table into prose.
        for rect in partial_grids(page) {
            if !objects
                .iter()
                .any(|(old, _)| old.intersect(rect).area() > rect.area() * 0.5)
            {
                regions.push(Region {
                    bounds: rect,
                    image_sources: Vec::new(),
                    recovery: true,
                });
            }
        }
        merge_regions(&mut regions, body * 0.5);
        if !regions.is_empty() {
            figures::group_subfigures(
                &mut regions,
                &lines(page.glyphs.iter().filter(|g| remaining[g.index]).cloned()),
                body,
            );
        }
        for region in regions {
            let rect = region.bounds.inflate(2.0, 2.0).intersect(Rect::new(
                0.0,
                0.0,
                page.width,
                page.height,
            ));
            let glyphs = page
                .glyphs
                .iter()
                .filter(|g| remaining[g.index] && rect.contains(g.bounds().center()))
                .cloned()
                .collect::<Vec<_>>();
            for glyph in &glyphs {
                remaining[glyph.index] = false;
            }
            let block = builder.image(rect, &glyphs, false);
            if !region.recovery {
                builder.crops.last_mut().unwrap().image_sources = region.image_sources;
            }
            stats.fallback_regions += 1;
            objects.push((rect, block));
        }
        let mut prose = lines(page.glyphs.iter().filter(|g| remaining[g.index]).cloned());
        // Confident superscript/bottom-note matches become the same Note IR the
        // EPUB and OCR readers use. Unmatched small print stays visible.
        let notes = extract_notes(&mut builder, &mut prose, page, body, stats);
        let headings = headings::recover(&mut builder, &mut prose, &page.headings);
        let mut items = prose
            .into_iter()
            .map(|line| (line.rect, Item::Line(line)))
            .chain(
                objects
                    .into_iter()
                    .map(|(rect, block)| (rect, Item::Block(block))),
            )
            .chain(headings)
            .collect::<Vec<_>>();
        order(&mut items, page.width, body);
        let mut paragraph: Vec<Line> = Vec::new();
        for (rect, item) in items {
            match item {
                Item::Block(block) => {
                    flush(&mut builder, &mut paragraph);
                    builder.section.blocks.push(block);
                }
                Item::Heading(blocks) => {
                    flush(&mut builder, &mut paragraph);
                    builder.section.blocks.extend(blocks);
                }
                Item::Line(line) => {
                    let heading = line.heading(body);
                    let math = line
                        .glyphs
                        .iter()
                        .filter(|g| {
                            g.text
                                .chars()
                                .any(|c| matches!(c, '∑' | '∫' | '√' | '∂' | '∏'))
                        })
                        .count()
                        > 0
                        && line.glyphs.iter().any(|g| script_size(g.size, line.size));
                    if math || line.glyphs.iter().any(|g| g.unmapped) {
                        flush(&mut builder, &mut paragraph);
                        let block = builder.image(
                            rect.inflate(body * 0.3, body * 0.3).intersect(Rect::new(
                                0.0,
                                0.0,
                                page.width,
                                page.height,
                            )),
                            &line.glyphs,
                            math,
                        );
                        builder.section.blocks.push(block);
                        stats.fallback_regions += 1;
                        continue;
                    }
                    if let Some(level) = heading {
                        flush(&mut builder, &mut paragraph);
                        let block = builder.text(&[line], TextBlockKind::Heading(level));
                        builder.section.blocks.push(Block::Text(block));
                        continue;
                    }
                    if paragraph.last().is_some_and(|previous| {
                        if is_figure_caption(&builder, &paragraph) {
                            !figure_caption_continues(&paragraph, &line, body)
                        } else {
                            !continues(previous, &line, body)
                        }
                    }) {
                        flush(&mut builder, &mut paragraph);
                    }
                    paragraph.push(line);
                }
            }
        }
        flush(&mut builder, &mut paragraph);
        builder.section.blocks.extend(notes);
        attach_captions(&mut builder.section.blocks);
    }
    let source = builder
        .section
        .blocks
        .first()
        .and_then(block_source)
        .map(|range| range.start.clone())
        .unwrap_or(SourceAnchor {
            spine: builder.section.id.clone(),
            node: "page".into(),
            text_offset: 0,
        });
    builder.section.anchors.push(SectionAnchor {
        fragment: format!("{PAGE_ANCHOR_PREFIX}{}", index + 1),
        source,
    });
    // This invariant detects accidental overlap between image/table/prose paths.
    let unique = builder
        .provenance
        .iter()
        .map(|p| p.original_glyph)
        .collect::<HashSet<_>>();
    if unique.len() != builder.provenance.len() || unique.len() + removed != page.glyphs.len() {
        return Err("PDF reflow lost or duplicated source glyphs".into());
    }
    Ok((
        StoredSection {
            section: builder.section,
            provenance: builder.provenance,
        },
        builder.crops,
    ))
}

pub(super) fn requires_page_fallback(page: &NativePage) -> bool {
    let rotated = page.glyphs.iter().filter(|g| g.rotated).count();
    let scanned = page
        .images
        .iter()
        .any(|r| (r[2] - r[0]) * (r[3] - r[1]) > page.width * page.height * 0.85);
    let rtl = page
        .glyphs
        .iter()
        .filter(|g| {
            g.text
                .chars()
                .any(|c| matches!(c as u32,0x0590..=0x08ff|0xfb1d..=0xfdff|0xfe70..=0xfeff))
        })
        .count();
    page.glyphs.len() < 3
        || super::quality::bad_text(page)
        || page.unmapped * 10 > page.glyphs.len().max(1)
        || rotated * 20 > page.glyphs.len().max(1)
        || scanned
        || rtl * 20 > page.glyphs.len().max(1)
}

enum Item {
    Line(Line),
    Block(Block),
    Heading(Vec<Block>),
}

fn order(items: &mut Vec<(Rect, Item)>, width: f64, body: f64) {
    items.sort_by(|(a, _), (b, _)| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
    let middle = width * 0.5;
    let left = items
        .iter()
        .filter(|(r, _)| r.x1 < middle - body * 0.3)
        .count();
    let right = items
        .iter()
        .filter(|(r, _)| r.x0 > middle + body * 0.3)
        .count();
    if left < 4 || right < 4 {
        return;
    }
    // Spanning objects split vertical bands. Read each band's left column then
    // its right column, never interleave baselines from the two columns.
    let mut sorted = Vec::new();
    let mut band = Vec::new();
    for item in std::mem::take(items) {
        if item.0.x0 < middle && item.0.x1 > middle {
            sort_band(&mut band, middle);
            sorted.append(&mut band);
            sorted.push(item);
        } else {
            band.push(item);
        }
    }
    sort_band(&mut band, middle);
    sorted.append(&mut band);
    *items = sorted;
}

fn sort_band(items: &mut [(Rect, Item)], middle: f64) {
    items.sort_by(|(a, _), (b, _)| {
        (a.center().x > middle)
            .cmp(&(b.center().x > middle))
            .then(a.y0.total_cmp(&b.y0))
            .then(a.x0.total_cmp(&b.x0))
    });
}

fn continues(previous: &Line, next: &Line, body: f64) -> bool {
    let gap = next.baseline - previous.baseline;
    let previous_bounds = previous.prose_bounds();
    let next_bounds = next.prose_bounds();
    let indent = next_bounds.x0 - previous_bounds.x0;
    gap > body * 0.4
        && gap < body * 1.8
        && (previous.size - next.size).abs() < body * 0.15
        // A first-line indent (or a large margin ordinal beside the first two
        // rows) may end as the paragraph continues. A new rightward indent
        // still starts a paragraph, and column ordering remains separate.
        && indent > -body * 3.1
        && indent < body * 1.1
        && next_bounds.x0 < previous_bounds.x1
        && !next.text().trim_start().starts_with(['•', '●', '▪'])
        && !(previous_bounds.width() < next_bounds.width() * 0.72
            && previous
                .text()
                .trim_end()
                .ends_with(['.', '!', '?', '。', '！', '？']))
}

// A marginal figure label is part of the caption, not its continuation indent.
// Keep this exception local to text directly following a real image.
fn is_figure_caption(builder: &Builder, paragraph: &[Line]) -> bool {
    matches!(builder.section.blocks.last(), Some(Block::Image(_)))
        && paragraph
            .first()
            .is_some_and(|line| caption_prefix_len(&line.text()).is_some())
}

fn figure_caption_continues(paragraph: &[Line], next: &Line, body: f64) -> bool {
    let Some(first) = paragraph.first() else {
        return false;
    };
    let Some(prefix) = caption_prefix_len(&first.text()) else {
        return false;
    };
    let previous = paragraph.last().unwrap();
    let mut offset = 0;
    let caption_left = first.glyphs.iter().find_map(|glyph| {
        let start = offset;
        offset += glyph.text.chars().count();
        (start >= prefix && !glyph.text.chars().all(char::is_whitespace)).then_some(glyph.rect[0])
    });
    let gap = next.baseline - previous.baseline;
    caption_left.is_some_and(|left| (left - next.prose_bounds().x0).abs() < body * 0.8)
        && gap > body * 0.4
        && gap < body * 1.8
        && first.size <= body * 1.1
        && (first.size - next.size).abs() <= body * 0.035
        && next.heading(body).is_none()
        && caption_prefix_len(&next.text()).is_none()
        && !next.text().trim_start().starts_with(['•', '●', '▪'])
}

fn caption_prefix_len(text: &str) -> Option<usize> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();
    let label = ["figure", "fig.", "图"]
        .into_iter()
        .find(|prefix| lower.starts_with(prefix))?;
    let rest = &trimmed[label.len()..];
    if label.chars().all(|c| c.is_ascii_alphabetic()) && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let number = rest.trim_start();
    if !number.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let digits = number
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '–'))
        .count();
    Some(text.chars().count() - number.chars().count() + digits)
}

fn caption_hyphen_join(previous: &[TextRun], next: &Line, words: &HashSet<String>) -> bool {
    let left = previous
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>();
    crate::reflow::proven_hyphen_join(&left, &next.text(), words)
}

#[derive(Clone, Copy)]
pub(super) struct BodyFrame {
    pub height: f64,
    pub left: f64,
    pub right: f64,
}

pub(super) fn body_frame(page: &NativePage, body: f64) -> Option<BodyFrame> {
    let prose = lines(page.glyphs.clone())
        .into_iter()
        .filter(|line| {
            line.heading(body).is_none()
                && (line.size - body).abs() <= body * 0.035
                && line.baseline > page.height * 0.15
                && line.baseline < page.height * 0.9
                && line.prose_bounds().width() > body * 8.0
        })
        .collect::<Vec<_>>();
    if prose.len() < 2 {
        return None;
    }
    let mut lefts = prose
        .iter()
        .map(|l| l.prose_bounds().x0)
        .collect::<Vec<_>>();
    let mut rights = prose
        .iter()
        .map(|l| l.prose_bounds().x1)
        .collect::<Vec<_>>();
    lefts.sort_by(f64::total_cmp);
    rights.sort_by(f64::total_cmp);
    let left = lefts[prose.len() / 5];
    let right = rights[prose.len() * 4 / 5];
    // Do not infer one text frame from two independent columns or small labels.
    (right - left > body * 12.0
        && prose
            .iter()
            .filter(|l| l.prose_bounds().width() >= (right - left) * 0.72)
            .count()
            * 5
            >= prose.len() * 3)
        .then_some(BodyFrame {
            height: page.height,
            left,
            right,
        })
}

fn flush(builder: &mut Builder, paragraph: &mut Vec<Line>) {
    if paragraph.is_empty() {
        return;
    }
    let first = paragraph[0].text();
    let leading = first.trim_start();
    let number: String = leading.chars().take_while(char::is_ascii_digit).collect();
    let kind = if is_figure_caption(builder, paragraph) {
        TextBlockKind::Caption
    } else if leading.starts_with(['•', '●', '▪', '◦']) {
        TextBlockKind::ListItem {
            ordered: false,
            ordinal: 1,
            depth: 0,
            marker_visible: false,
        }
    } else if !number.is_empty()
        && number.len() < 4
        && leading[number.len()..].starts_with(['.', ')'])
        && !(leading[number.len()..].starts_with('.')
            && leading[number.len() + 1..].starts_with(|c: char| c.is_ascii_digit()))
    {
        TextBlockKind::ListItem {
            ordered: true,
            ordinal: number.parse().unwrap_or(1),
            depth: 0,
            marker_visible: false,
        }
    } else {
        TextBlockKind::Paragraph
    };
    let block = builder.text(paragraph, kind);
    builder.section.blocks.push(Block::Text(block));
    paragraph.clear();
}

fn cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x30ff | 0x3400..=0x9fff | 0xac00..=0xd7af | 0xff00..=0xffef)
}

fn merge_regions(regions: &mut Vec<Region>, tolerance: f64) {
    let mut i = 0;
    while i < regions.len() {
        let mut j = i + 1;
        while j < regions.len() {
            if regions[i]
                .bounds
                .inflate(tolerance, tolerance)
                .intersect(regions[j].bounds)
                .area()
                > 0.0
            {
                let other = regions.remove(j);
                regions[i].merge(other);
                j = i + 1;
            } else {
                j += 1;
            }
        }
        i += 1;
    }
}

fn grids(page: &NativePage) -> Vec<(Rect, Vec<f64>, Vec<f64>)> {
    let horizontal: Vec<_> = page
        .rules
        .iter()
        .filter(|r| r[3] - r[1] < 1.0 && r[2] - r[0] > 25.0)
        .collect();
    let vertical: Vec<_> = page
        .rules
        .iter()
        .filter(|r| r[2] - r[0] < 1.0 && r[3] - r[1] > 15.0)
        .collect();
    let mut results: Vec<(Rect, Vec<f64>, Vec<f64>)> = Vec::new();
    for top in &horizontal {
        let ys = horizontal
            .iter()
            .filter(|r| (r[0] - top[0]).abs() < 2.0 && (r[2] - top[2]).abs() < 2.0)
            .map(|r| r[1])
            .collect::<Vec<_>>();
        let ys = distinct(ys);
        if ys.len() < 3 {
            continue;
        }
        let y0 = ys[0];
        let y1 = *ys.last().unwrap();
        let xs = distinct(
            vertical
                .iter()
                .filter(|r| {
                    r[1] <= y0 + 2.0
                        && r[3] >= y1 - 2.0
                        && r[0] >= top[0] - 2.0
                        && r[0] <= top[2] + 2.0
                })
                .map(|r| r[0])
                .collect(),
        );
        if xs.len() < 3 || xs.len() > 30 || ys.len() > 100 {
            continue;
        }
        let rect = Rect::new(xs[0], y0, *xs.last().unwrap(), y1);
        if !results
            .iter()
            .any(|(old, _, _)| old.intersect(rect).area() > rect.area() * 0.5)
        {
            results.push((rect, xs, ys));
        }
    }
    results
}

fn partial_grids(page: &NativePage) -> Vec<Rect> {
    let horizontal: Vec<_> = page
        .rules
        .iter()
        .filter(|r| r[3] - r[1] < 1.0 && r[2] - r[0] > 40.0)
        .collect();
    let mut result: Vec<Rect> = Vec::new();
    for line in &horizontal {
        let ys = distinct(
            horizontal
                .iter()
                .filter(|r| (r[0] - line[0]).abs() < 2.0 && (r[2] - line[2]).abs() < 2.0)
                .map(|r| r[1])
                .collect(),
        );
        if ys.len() < 3 {
            continue;
        }
        let rect = Rect::new(line[0], ys[0], line[2], *ys.last().unwrap());
        let vertical = page
            .rules
            .iter()
            .filter(|r| {
                r[2] - r[0] < 1.0
                    && r[0] >= rect.x0 - 2.0
                    && r[0] <= rect.x1 + 2.0
                    && r[1] >= rect.y0 - 2.0
                    && r[3] <= rect.y1 + 2.0
                    && r[3] - r[1] > 10.0
            })
            .count();
        if vertical >= 3
            && !result
                .iter()
                .any(|old| old.intersect(rect).area() > rect.area() * 0.5)
        {
            result.push(rect);
        }
    }
    result
}

fn distinct(mut values: Vec<f64>) -> Vec<f64> {
    values.sort_by(f64::total_cmp);
    values.dedup_by(|a, b| (*a - *b).abs() < 2.0);
    values
}

fn borderless_grids(page: &NativePage, body: f64) -> Vec<(Rect, Vec<f64>, Vec<f64>)> {
    let mut all = lines(page.glyphs.clone());
    all.sort_by(|a, b| {
        a.baseline
            .total_cmp(&b.baseline)
            .then(a.rect.x0.total_cmp(&b.rect.x0))
    });
    let mut rows: Vec<Vec<Line>> = Vec::new();
    for line in all {
        if let Some(row) = rows
            .last_mut()
            .filter(|r| (r[0].baseline - line.baseline).abs() < body * 0.35)
        {
            row.push(line);
        } else {
            rows.push(vec![line]);
        }
    }
    let mut found = Vec::new();
    for (index, header) in rows.iter().enumerate() {
        if !(2..=6).contains(&header.len())
            || !header
                .iter()
                .all(|l| l.text().chars().count() <= 50 && l.glyphs.iter().all(|g| g.bold))
        {
            continue;
        }
        let mut end = index + 1;
        while end < rows.len()
            && rows[end].len() == header.len()
            && rows[end].iter().zip(header).all(|(a, b)| {
                (a.rect.x0 - b.rect.x0).abs() < body * 0.5 && a.text().chars().count() <= 80
            })
            && rows[end][0].baseline - rows[end - 1][0].baseline < body * 2.5
        {
            end += 1;
        }
        if end - index < 3 {
            continue;
        }
        let bounds = rows[index..end]
            .iter()
            .flatten()
            .map(|l| l.rect)
            .reduce(|a, b| a.union(b))
            .unwrap();
        // Aligned body columns without a bold header cannot qualify here.
        let mut xs = vec![bounds.x0 - body * 0.2];
        for column in 1..header.len() {
            let previous = rows[index..end]
                .iter()
                .map(|r| r[column - 1].rect.x1)
                .fold(0.0, f64::max);
            let next = header[column].rect.x0;
            if next - previous < body {
                xs.clear();
                break;
            }
            xs.push((previous + next) * 0.5);
        }
        if xs.is_empty() {
            continue;
        }
        xs.push(bounds.x1 + body * 0.2);
        let mut ys = vec![bounds.y0 - body * 0.1];
        for row in index + 1..end {
            ys.push((rows[row - 1][0].rect.y1 + rows[row][0].rect.y0) * 0.5);
        }
        ys.push(bounds.y1 + body * 0.1);
        found.push((
            Rect::new(xs[0], ys[0], *xs.last().unwrap(), *ys.last().unwrap()),
            xs,
            ys,
        ));
    }
    found
}

fn extract_notes(
    builder: &mut Builder,
    lines: &mut Vec<Line>,
    page: &NativePage,
    body: f64,
    stats: &mut Statistics,
) -> Vec<Block> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        if line.rect.y0 < page.height * 0.72 || line.size >= body * 0.91 {
            index += 1;
            continue;
        }
        let label: String = line
            .text()
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if label.is_empty() || label.len() > 3 {
            index += 1;
            continue;
        }
        let markers = lines
            .iter()
            .take(index)
            .flat_map(|l| {
                l.glyphs.iter().filter(|g| {
                    g.text == label
                        && g.size < l.size * 0.85
                        && g.baseline[1] < l.baseline - l.size * 0.12
                })
            })
            .map(|g| g.index)
            .collect::<HashSet<_>>();
        if markers.is_empty() {
            index += 1;
            continue;
        }
        let label: String = line
            .text()
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let mut end = index + 1;
        while end < lines.len()
            && lines[end].size < body * 0.91
            && !lines[end]
                .text()
                .trim_start()
                .starts_with(|c: char| c.is_ascii_digit())
            && lines[end].baseline - lines[end - 1].baseline < body * 1.6
        {
            end += 1;
        }
        let note_lines: Vec<_> = lines.drain(index..end).collect();
        let fragment = format!("pdf-native-note-{}-{label}", builder.page + 1);
        let target =
            PublicationUrl::parse(&format!("{}#{fragment}", builder.section.href)).unwrap();
        for glyph in lines.iter_mut().flat_map(|l| &mut l.glyphs) {
            if markers.contains(&glyph.index) {
                glyph.link = Some(target.to_string());
            }
        }
        let note = builder.text(&note_lines, TextBlockKind::FootnoteDefinition);
        if let Some(source) = &note.source {
            builder.section.anchors.push(SectionAnchor {
                fragment,
                source: source.start.clone(),
            });
        }
        output.push(Block::Note(NoteBlock {
            kind: NoteBlockKind::Definition,
            source: note.source.clone(),
            blocks: vec![Block::Text(note)],
        }));
        stats.notes += 1;
    }
    output
}

fn block_source(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(b) => b.source.as_ref(),
        Block::Table(b) => b.source.as_ref(),
        Block::Image(b) => b.source.as_ref(),
        Block::Figure(b) => b.source.as_ref(),
        Block::Note(b) => b.source.as_ref(),
        _ => None,
    }
}

fn attach_captions(blocks: &mut Vec<Block>) {
    let mut index = 0;
    while index < blocks.len() {
        let caption = if let Block::Text(text) = &blocks[index] {
            let plain = text
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
            plain.trim_start().to_lowercase().starts_with("table ")
                || plain.trim_start().starts_with('表')
        } else {
            false
        };
        if caption && matches!(blocks.get(index + 1), Some(Block::Table(_))) {
            let Block::Text(mut text) = blocks.remove(index) else {
                unreachable!()
            };
            text.kind = TextBlockKind::Caption;
            if let Block::Table(table) = &mut blocks[index] {
                table.before.push(text);
            }
        }
        index += 1;
    }
    let mut index = 0;
    while index + 1 < blocks.len() {
        let caption = if let Some(Block::Text(text)) = blocks.get(index + 1) {
            let plain = text
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
            let plain = plain.trim_start().to_lowercase();
            plain.starts_with("figure ") || plain.starts_with("fig.") || plain.starts_with('图')
        } else {
            false
        };
        if caption && matches!(blocks[index], Block::Image(_)) {
            let Block::Image(image) = blocks.remove(index) else {
                unreachable!()
            };
            let Block::Text(mut caption) = blocks.remove(index) else {
                unreachable!()
            };
            caption.kind = TextBlockKind::Caption;
            let source = image.source.clone();
            blocks.insert(
                index,
                Block::Figure(FigureBlock {
                    images: vec![image],
                    captions: vec![caption],
                    caption_position: CaptionPosition::After,
                    style: BlockStyle::default(),
                    source,
                }),
            );
        }
        let note = if let Some(Block::Text(text)) = blocks.get(index + 1) {
            let plain = text
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
            ["Note:", "Notes:", "Source:", "注：", "来源："]
                .iter()
                .any(|prefix| plain.trim_start().starts_with(prefix))
        } else {
            false
        };
        if note && matches!(blocks[index], Block::Table(_)) {
            let Block::Text(mut note) = blocks.remove(index + 1) else {
                unreachable!()
            };
            note.kind = TextBlockKind::Caption;
            if let Block::Table(table) = &mut blocks[index] {
                table.after.push(note);
            }
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn glyph(text: &str, x: f64, y: f64, size: f64, index: usize) -> NativeGlyph {
        NativeGlyph {
            text: text.into(),
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
            index,
            unmapped: false,
        }
    }

    fn row(text: &str, x: f64, y: f64, size: f64, glyphs: &mut Vec<NativeGlyph>) {
        for (offset, c) in text.chars().enumerate() {
            glyphs.push(glyph(
                &c.to_string(),
                x + offset as f64 * size * 0.5,
                y,
                size,
                glyphs.len(),
            ));
        }
    }

    #[test]
    fn singleton_header_requires_outline_counter_and_proven_layout() {
        let header_page = |index: usize, label: &str, counter: u32, shift: f64, heading: bool| {
            let mut page = NativePage {
                width: 600.0,
                height: 800.0,
                ..NativePage::default()
            };
            let text = format!("{label} {counter}");
            let left = 500.0 - text.chars().count() as f64 * 4.5 + shift;
            row(&text, left, 105.0, 9.0, &mut page.glyphs);
            if heading {
                for glyph in &mut page.glyphs {
                    glyph.tag = Some("H3".into());
                }
            }
            row(
                "Ordinary body text starts here.",
                100.0,
                145.0,
                10.0,
                &mut page.glyphs,
            );
            assert!(index >= 69);
            page
        };
        let supported = |candidate: &NativePage| {
            let mut detector = ChromeDetector::default();
            for (index, label) in [
                (90, "2.2 Computer Hardware"),
                (92, "2.2 Computer Hardware"),
                (96, "2.4 Important Systems"),
                (98, "2.4 Important Systems"),
            ] {
                detector.observe(
                    &header_page(index, label, (index + 1 - 69) as u32, 0.0, false),
                    index,
                );
            }
            detector.observe(candidate, 94);
            detector.finish();
            detector
        };
        let candidate = header_page(94, "2.3 Display Technologies", 26, 0.0, false);
        let detector = supported(&candidate);
        let labels = ["2.3 Display Technologies"];
        assert_eq!(
            detector.removed_keys(94, &labels),
            edge_signatures(&candidate)
        );
        assert!(
            detector
                .removed_keys(94, &["2.2 Computer Hardware"])
                .is_empty()
        );
        for candidate in [
            header_page(94, "2.3 Display Technologies", 777, 0.0, false),
            header_page(94, "2.3 Display Technologies", 26, 10.0, false),
            header_page(94, "2.3 Display Technologies", 26, 0.0, true),
        ] {
            assert!(supported(&candidate).removed_keys(94, &labels).is_empty());
        }
    }

    #[test]
    fn rounded_eighty_percent_superscript_does_not_split_prose() {
        let mut glyphs = Vec::new();
        row("Text", 50.0, 156.1846, 9.9626, &mut glyphs);
        glyphs.push(glyph("2", 70.0, 152.449, 7.9701, glyphs.len()));
        row("continues", 50.0, 170.1316, 9.9626, &mut glyphs);
        let grouped = lines(glyphs);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].text(), "Text2");
        assert_eq!(grouped[1].text(), "continues");
    }

    #[test]
    fn mirrored_page_join_preserves_real_paragraph_boundaries() {
        for (indent, terminal, expected_join) in
            [(0.0, false, true), (20.0, false, false), (0.0, true, false)]
        {
            let mut previous = NativePage {
                width: 600.0,
                height: 800.0,
                ..NativePage::default()
            };
            let tail = if terminal {
                format!("{}.", "a".repeat(79))
            } else {
                "a".repeat(80)
            };
            row(&tail, 100.0, 790.0, 10.0, &mut previous.glyphs);
            let mut next = NativePage {
                width: 600.0,
                height: 800.0,
                ..NativePage::default()
            };
            row(
                "continued text",
                80.0 + indent,
                140.0,
                10.0,
                &mut next.glyphs,
            );
            let (mut old, _) = build(
                &previous,
                0,
                10.0,
                &HashSet::new(),
                &mut Statistics::default(),
            )
            .unwrap();
            let (mut new, _) =
                build(&next, 1, 10.0, &HashSet::new(), &mut Statistics::default()).unwrap();
            super::super::relocate(&mut new, old.section.id.clone(), old.section.href.clone());
            super::super::join_page_paragraph(
                &mut old,
                &mut new,
                &next,
                10.0,
                Some(BodyFrame {
                    height: 800.0,
                    left: 100.0,
                    right: 500.0,
                }),
                Some(BodyFrame {
                    height: 800.0,
                    left: 80.0,
                    right: 480.0,
                }),
            );
            assert_eq!(
                new.section.blocks.is_empty(),
                expected_join,
                "indent={indent}, terminal={terminal}"
            );
        }
    }

    #[test]
    fn large_section_number_cannot_interleave_two_body_rows() {
        let mut glyphs = vec![glyph("2.1", 150.0, 314.638, 28.8917, 0)];
        row(
            "This chapter provides a brief history",
            178.56,
            314.638,
            9.9626,
            &mut glyphs,
        );
        row(
            "understanding their interaction techniques",
            178.56,
            328.58564,
            9.9626,
            &mut glyphs,
        );
        // Also exercise content streams that paint the second row first.
        glyphs.reverse();
        let grouped = lines(glyphs);
        assert_eq!(grouped.len(), 2);
        assert_eq!(
            grouped[0].text(),
            "2.1This chapter provides a brief history"
        );
        assert_eq!(
            grouped[1].text(),
            "understanding their interaction techniques"
        );
        assert_eq!(grouped[0].size, 9.9626);
        assert_eq!(grouped[0].baseline, 314.638);
        assert_eq!(grouped[0].heading(9.9626), None);
        let indices = grouped
            .iter()
            .flat_map(|l| l.glyphs.iter().map(|g| g.index))
            .collect::<Vec<_>>();
        assert_eq!(indices.iter().collect::<HashSet<_>>().len(), indices.len());
    }

    #[test]
    fn same_baseline_mixed_sizes_stay_together_without_baseline_drift() {
        let mut glyphs = Vec::new();
        row("Body", 50.0, 100.0, 12.0, &mut glyphs);
        glyphs.push(glyph("BIG", 75.0, 100.8, 20.0, glyphs.len()));
        glyphs.push(glyph("tail", 90.0, 101.5, 12.0, glyphs.len()));
        glyphs.push(glyph("other", 50.0, 116.0, 12.0, glyphs.len()));
        let grouped = lines(glyphs);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].text(), "BodyBIGtail");
        assert_eq!(grouped[0].baseline, 100.8);
        assert_eq!(grouped[1].text(), "other");
    }

    #[test]
    fn small_scripts_attach_but_separate_small_print_does_not() {
        let mut glyphs = Vec::new();
        row("Text", 50.0, 100.0, 12.0, &mut glyphs);
        glyphs.push(glyph("1", 75.0, 95.0, 7.0, glyphs.len()));
        glyphs.push(glyph("2", 79.0, 104.0, 7.0, glyphs.len()));
        row("small print", 50.0, 107.0, 7.0, &mut glyphs);
        row("Next row", 50.0, 116.0, 12.0, &mut glyphs);
        let grouped = lines(glyphs);
        assert_eq!(
            grouped.iter().map(Line::text).collect::<Vec<_>>(),
            ["Text12", "small print", "Next row"]
        );
        let mut builder = Builder {
            section: Section {
                id: spine_id(0).unwrap(),
                href: section_href(0).unwrap(),
                blocks: Vec::new(),
                anchors: Vec::new(),
            },
            provenance: Vec::new(),
            crops: Vec::new(),
            page: 0,
            body: 12.0,
            width: 600.0,
            height: 800.0,
        };
        let block = builder.text(&grouped[..1], TextBlockKind::Paragraph);
        assert!(block.content.iter().any(|i| matches!(i, Inline::Text(t) if t.text == "1" && t.style.baseline == TextBaseline::Superscript)));
        assert!(block.content.iter().any(|i| matches!(i, Inline::Text(t) if t.text == "2" && t.style.baseline == TextBaseline::Subscript)));
    }

    #[test]
    fn drifting_baselines_cannot_snowball_into_one_row() {
        let glyphs = (0..8).map(|i| glyph("x", 50.0 + i as f64 * 6.0, 100.0 + i as f64, 12.0, i));
        let grouped = lines(glyphs);
        assert!(grouped.len() > 1);
        for line in grouped {
            let lo = line
                .glyphs
                .iter()
                .map(|g| g.baseline[1])
                .fold(f64::INFINITY, f64::min);
            let hi = line
                .glyphs
                .iter()
                .map(|g| g.baseline[1])
                .fold(f64::NEG_INFINITY, f64::max);
            assert!(hi - lo <= 3.0);
        }
    }

    #[test]
    fn decimal_section_prefix_is_not_an_ordered_list_marker() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        row(
            "2.1 This chapter describes computers.",
            50.0,
            100.0,
            12.0,
            &mut page.glyphs,
        );
        let (stored, _) =
            build(&page, 0, 12.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        let Block::Text(t) = &stored.section.blocks[0] else {
            panic!()
        };
        assert_eq!(t.kind, TextBlockKind::Paragraph);
        page.glyphs.clear();
        row("1)2 + 2 equals four.", 50.0, 100.0, 12.0, &mut page.glyphs);
        let (stored, _) =
            build(&page, 0, 12.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert!(
            matches!(&stored.section.blocks[0], Block::Text(t) if matches!(t.kind, TextBlockKind::ListItem { ordered: true, .. }))
        );
    }

    #[test]
    fn paragraph_indent_can_end_without_absorbing_the_next_paragraph() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        row(
            "An indented first line continues",
            74.0,
            100.0,
            12.0,
            &mut page.glyphs,
        );
        row("on a flush-left row.", 50.0, 115.0, 12.0, &mut page.glyphs);
        row(
            "The next paragraph starts here.",
            74.0,
            130.0,
            12.0,
            &mut page.glyphs,
        );
        let (stored, _) =
            build(&page, 0, 12.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert_eq!(stored.section.blocks.len(), 2);
        let Block::Text(first) = &stored.section.blocks[0] else {
            panic!()
        };
        let text = first
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(
            text,
            "An indented first line continues on a flush-left row."
        );
    }

    #[test]
    #[ignore = "Set TORTO_REFLOW_TEST_PDF to the local Pick, Click, Flick! PDF"]
    fn local_pdf_marginal_figure_caption() {
        let path = std::env::var_os("TORTO_REFLOW_TEST_PDF").expect("local PDF path");
        let publication = crate::pdf::open(fs::read(path).unwrap(), "local-probe.pdf").unwrap();
        let tags = extract::structure_tags(&publication.pdf);
        let page = extract::page(&publication.pdf, 97, &tags);
        let (stored, _) = build(
            &page,
            97,
            10.0,
            &edge_signatures(&page),
            &mut Statistics::default(),
        )
        .unwrap();
        let figure = stored
            .section
            .blocks
            .iter()
            .find_map(|b| {
                if let Block::Figure(f) = b {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("Sketchpad figure");
        assert_eq!(figure.captions.len(), 1);
        let caption = plain(&figure.captions[0]);
        assert!(
            caption.contains("Timothy Johnson, who created"),
            "{caption}"
        );
        assert!(
            caption.ends_with("Sketchpad video [Sutherland 1964]."),
            "{caption}"
        );
        assert!(
            !stored
                .section
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Text(t) if plain(t).starts_with("son,")))
        );
        assert!(
            stored
                .section
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Text(t) if plain(t).starts_with("for the TX-2")))
        );
        let unique = stored
            .provenance
            .iter()
            .map(|p| p.original_glyph)
            .collect::<HashSet<_>>();
        assert_eq!(unique.len(), stored.provenance.len());
        eprintln!("Verified complete Sketchpad caption: {caption}");
    }

    #[test]
    fn subfigure_group_keeps_labels_caption_url_and_body_boundary() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        page.images
            .extend([[140.0, 150.0, 260.0, 290.0], [330.0, 210.0, 440.0, 290.0]]);
        row("(a)", 190.0, 302.0, 10.0, &mut page.glyphs);
        row("(b)", 370.0, 302.0, 10.0, &mut page.glyphs);
        row("Figure 2.15 ", 80.0, 322.0, 8.0, &mut page.glyphs);
        row(
            "(a) Lisa and (b) its mouse. Source:",
            140.0,
            322.0,
            9.5,
            &mut page.glyphs,
        );
        let url_start = page.glyphs.len();
        row(
            "https://example.org, courtesy Museum.",
            140.0,
            334.0,
            9.5,
            &mut page.glyphs,
        );
        for glyph in &mut page.glyphs[url_start..url_start + "https://example.org".len()] {
            glyph.link = Some("https://example.org".into());
        }
        row(
            "The body resumes here.",
            140.0,
            348.0,
            10.0,
            &mut page.glyphs,
        );
        let (stored, crops) =
            build(&page, 0, 10.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert_eq!(crops.len(), 1, "{crops:?}");
        let mut sources = crops[0].image_sources.clone();
        sources.sort_unstable();
        assert_eq!(sources, vec![0, 1], "both subimages retain their identity");
        assert!(crops[0].bounds.contains(Point::new(190.0, 300.0)));
        assert!(crops[0].bounds.contains(Point::new(370.0, 300.0)));
        assert!(
            crops[0].bounds.y1 < 314.0,
            "caption remains text outside the crop"
        );
        assert_eq!(stored.section.blocks.len(), 2);
        let Block::Figure(figure) = &stored.section.blocks[0] else {
            panic!("expected grouped figure");
        };
        assert_eq!(figure.images.len(), 1);
        assert_eq!(figure.captions.len(), 1);
        let expected_url = PublicationUrl::website("https://example.org").unwrap();
        assert!(figure.captions[0].content.iter().any(
            |inline| matches!(inline, Inline::Text(run) if run.link.as_ref() == Some(&expected_url))
        ));
        assert_eq!(
            plain(&figure.captions[0]),
            "Figure 2.15 (a) Lisa and (b) its mouse. Source: https://example.org, courtesy Museum."
        );
        let Block::Text(body) = &stored.section.blocks[1] else {
            panic!("expected separate body paragraph");
        };
        assert_eq!(plain(body), "The body resumes here.");
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
        for panel in 0..6 {
            assert!(
                stored
                    .provenance
                    .iter()
                    .any(|p| p.original_glyph == panel && p.source.start.node.contains("image"))
            );
        }
    }

    #[test]
    fn subfigure_group_requires_shared_caption_and_clear_geometry() {
        for obstacle in ["unreferenced", "body", "between", "region", "duplicate"] {
            let mut regions = vec![
                Region::graphic(Rect::new(100.0, 100.0, 200.0, 200.0)),
                Region::graphic(Rect::new(260.0, 100.0, 360.0, 200.0)),
            ];
            let mut glyphs = Vec::new();
            row("(a)", 140.0, 215.0, 10.0, &mut glyphs);
            row(
                if obstacle == "duplicate" {
                    "(a)"
                } else {
                    "(b)"
                },
                300.0,
                215.0,
                10.0,
                &mut glyphs,
            );
            row(
                if obstacle == "unreferenced" {
                    "Figure 1.2 A different figure."
                } else {
                    "Figure 1.2 (a) Left, (b) right."
                },
                90.0,
                240.0,
                9.0,
                &mut glyphs,
            );
            if obstacle == "body" {
                row("Body", 215.0, 170.0, 10.0, &mut glyphs);
            }
            if obstacle == "between" {
                row("Body", 215.0, 228.0, 9.0, &mut glyphs);
            }
            if obstacle == "region" {
                regions.push(Region::graphic(Rect::new(215.0, 140.0, 240.0, 170.0)));
            }
            let count = regions.len();
            figures::group_subfigures(&mut regions, &lines(glyphs), 10.0);
            assert_eq!(regions.len(), count, "{obstacle}");
        }
        // A single embedded picture can already contain both panels.
        let mut regions = vec![Region::graphic(Rect::new(100.0, 100.0, 360.0, 200.0))];
        let mut glyphs = Vec::new();
        row("(a)", 140.0, 215.0, 10.0, &mut glyphs);
        row("(b)", 300.0, 215.0, 10.0, &mut glyphs);
        row("Figure 1.2 (a) Left and", 90.0, 240.0, 9.0, &mut glyphs);
        row("(b) right.", 139.5, 252.0, 9.0, &mut glyphs);
        figures::group_subfigures(&mut regions, &lines(glyphs), 10.0);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].bounds.y1, 215.0);
    }

    #[test]
    fn overlapping_picture_regions_retain_sources_and_recovery_barriers() {
        let mut regions = vec![
            Region {
                bounds: Rect::new(100.0, 100.0, 200.0, 200.0),
                image_sources: vec![0],
                recovery: false,
            },
            Region {
                bounds: Rect::new(190.0, 100.0, 290.0, 200.0),
                image_sources: vec![1],
                recovery: false,
            },
        ];
        merge_regions(&mut regions, 5.0);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].image_sources, vec![0, 1]);
        assert!(!regions[0].recovery);
        regions.push(Region {
            bounds: Rect::new(285.0, 100.0, 390.0, 200.0),
            image_sources: Vec::new(),
            recovery: true,
        });
        merge_regions(&mut regions, 5.0);
        assert_eq!(regions.len(), 1);
        assert!(
            regions[0].recovery,
            "a recovered table cannot become a float through overlap"
        );
    }

    #[test]
    #[ignore = "Set TORTO_REFLOW_TEST_PDF to the local Pick, Click, Flick! PDF"]
    fn local_pdf_subfigure_captions() {
        let path = std::env::var_os("TORTO_REFLOW_TEST_PDF").expect("local PDF path");
        let publication = crate::pdf::open(fs::read(path).unwrap(), "local-probe.pdf").unwrap();
        let tags = extract::structure_tags(&publication.pdf);
        for (index, label, ending) in [
            (
                102,
                "Figure 2.12",
                "http://toastytech.com/guis/saltodraw.png.",
            ),
            (105, "Figure 2.14", "Courtesy DigiBarn Computer Museum."),
            (107, "Figure 2.15", "courtesy Digibarn Computer Museum."),
        ] {
            let page = extract::page(&publication.pdf, index, &tags);
            let mut stats = Statistics::default();
            let (stored, crops) =
                build(&page, index, 10.0, &edge_signatures(&page), &mut stats).unwrap();
            let figure = stored
                .section
                .blocks
                .iter()
                .find_map(|block| match block {
                    Block::Figure(figure)
                        if figure
                            .captions
                            .iter()
                            .any(|caption| plain(caption).starts_with(label)) =>
                    {
                        Some(figure)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing grouped {label}"));
            assert_eq!(figure.captions.len(), 1);
            let caption = plain(&figure.captions[0]);
            assert!(caption.ends_with(ending), "{caption}");
            assert!(!stored.section.blocks.iter().any(|block| matches!(block, Block::Text(text) if matches!(plain(text).as_str(), "(a)" | "(b)"))));
            assert_eq!(
                stored.provenance.len() + stats.removed_chrome_glyphs,
                page.glyphs.len()
            );
            assert_eq!(
                stored
                    .provenance
                    .iter()
                    .map(|p| p.original_glyph)
                    .collect::<HashSet<_>>()
                    .len(),
                stored.provenance.len()
            );
            eprintln!(
                "Verified {label} on PDF page {}: {caption}; crops={}",
                index + 1,
                crops.len()
            );
        }
    }

    #[test]
    fn marginal_caption_keeps_continuations_and_stops_before_body() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        page.images.push([120.0, 150.0, 300.0, 290.0]);
        row("Figure 1.2 ", 70.0, 310.0, 9.0, &mut page.glyphs);
        for glyph in &mut page.glyphs {
            glyph.bold = true;
        }
        row("Timothy John-", 120.0, 310.0, 9.0, &mut page.glyphs);
        row(
            "son made this [Johnson 1963], a well-",
            120.0,
            322.0,
            9.0,
            &mut page.glyphs,
        );
        row(
            "known prototype-instance example.",
            120.0,
            334.0,
            9.0,
            &mut page.glyphs,
        );
        // Same left edge and tight spacing: the body font still ends the caption.
        row(
            "The body resumes here.",
            120.0,
            346.0,
            10.0,
            &mut page.glyphs,
        );
        let (stored, _) =
            build(&page, 0, 10.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert_eq!(stored.section.blocks.len(), 2);
        let Block::Figure(figure) = &stored.section.blocks[0] else {
            panic!("expected figure");
        };
        let text = plain(&figure.captions[0]);
        assert!(text.contains("Timothy Johnson made this"), "{text}");
        assert!(text.contains("well-known prototype-instance"), "{text}");
        let Block::Text(body) = &stored.section.blocks[1] else {
            panic!("expected body");
        };
        assert_eq!(body.kind, TextBlockKind::Paragraph);
        assert_eq!(plain(body), "The body resumes here.");
        assert_eq!(stored.provenance.len(), page.glyphs.len());
        let source = figure.captions[0].source.as_ref().unwrap();
        let chars = figure.captions[0]
            .content
            .iter()
            .filter_map(|i| {
                if let Inline::Text(t) = i {
                    Some(t.text.chars().count())
                } else {
                    None
                }
            })
            .sum::<usize>();
        assert_eq!(source.end.text_offset, chars as u64);
        let removed = stored
            .provenance
            .iter()
            .filter(|p| {
                p.source.start.node == source.start.node
                    && p.source.start.text_offset == p.source.end.text_offset
            })
            .count();
        assert_eq!(
            removed, 1,
            "only the proven John-son split loses its hyphen"
        );
    }

    #[test]
    #[ignore = "Set TORTO_REFLOW_TEST_PDF to the local Pick, Click, Flick! PDF"]
    fn local_pdf_paragraph_continuation() {
        let path = std::env::var_os("TORTO_REFLOW_TEST_PDF").expect("local PDF path");
        let publication = crate::pdf::open(fs::read(path).unwrap(), "local-probe.pdf").unwrap();
        let tags = extract::structure_tags(&publication.pdf);
        let page = extract::page(&publication.pdf, 103, &tags);
        let (stored, _) = build(
            &page,
            103,
            10.0,
            &edge_signatures(&page),
            &mut Statistics::default(),
        )
        .unwrap();
        assert!(stored.section.blocks.iter().any(|b| matches!(b, Block::Text(t) if plain(t).starts_with("Warnock, Chuck Geschke") && plain(t).contains("Gargoyle editor"))));
        assert!(
            !stored
                .section
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Text(t) if matches!(plain(t).as_str(), "2"|"3")))
        );
        let old_page = extract::page(&publication.pdf, 97, &tags);
        let new_page = extract::page(&publication.pdf, 98, &tags);
        let old_frame = body_frame(&old_page, 10.0);
        let new_frame = body_frame(&new_page, 10.0);
        eprintln!(
            "Mirrored body shift: {:.3} points",
            old_frame.unwrap().left - new_frame.unwrap().left
        );
        let (mut old, _) = build(
            &old_page,
            97,
            10.0,
            &edge_signatures(&old_page),
            &mut Statistics::default(),
        )
        .unwrap();
        let (mut new, _) = build(
            &new_page,
            98,
            10.0,
            &edge_signatures(&new_page),
            &mut Statistics::default(),
        )
        .unwrap();
        relocate(&mut new, old.section.id.clone(), old.section.href.clone());
        let count = new.section.blocks.len();
        join_page_paragraph(&mut old, &mut new, &new_page, 10.0, old_frame, new_frame);
        assert_eq!(new.section.blocks.len(), count - 1);
        assert!(old.section.blocks.iter().any(|b| matches!(b, Block::Text(t) if plain(t).starts_with("Later additions to Sketchpad") && plain(t).contains("visual programming system"))));
        let owned = old
            .provenance
            .iter()
            .chain(&new.provenance)
            .map(|p| (p.page, p.original_glyph))
            .collect::<HashSet<_>>();
        assert_eq!(owned.len(), old.provenance.len() + new.provenance.len());
        eprintln!("Verified upper scripts on page 104 and paragraph continuity on pages 98-99");
    }

    #[test]
    #[ignore = "Set TORTO_REFLOW_TEST_PDF to the local Pick, Click, Flick! PDF"]
    fn local_pdf_running_headers_and_split_headings() {
        let path = std::env::var_os("TORTO_REFLOW_TEST_PDF").expect("local PDF path");
        let publication = crate::pdf::open(fs::read(path).unwrap(), "local-probe.pdf").unwrap();
        let tags = extract::structure_tags(&publication.pdf);
        let mut pages = Vec::new();
        let mut edges = ChromeDetector::default();
        for index in 89..127 {
            let mut page = extract::page(&publication.pdf, index, &tags);
            if index == 89 {
                mark_outline_headings(
                    &mut page,
                    &[
                        ("2 History of Desktop Devices".into(), 2),
                        ("2.1 Introduction".into(), 3),
                        ("2.2 Computer Hardware".into(), 3),
                    ],
                );
                assert_eq!(page.headings.len(), 3);
            }
            edges.observe(&page, index);
            pages.push(page);
        }
        edges.finish();
        let mut stats = Statistics::default();
        for (offset, page) in pages.iter().enumerate() {
            let index = offset + 89;
            let repeated = edges.removed_keys(
                index,
                &["2 History of Desktop Devices", "2.3 Display Technologies"],
            );
            let (stored, _) = build(page, index, 10.0, &repeated, &mut stats).unwrap();
            let text = stored
                .section
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text(t) => Some((t.kind, plain(t))),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if index == 89 {
                for expected in [
                    (TextBlockKind::HeadingOrdinal(2), "CHAPTER 2"),
                    (TextBlockKind::Heading(2), "History of Desktop Devices"),
                    (TextBlockKind::HeadingOrdinal(3), "2.1"),
                    (TextBlockKind::Heading(3), "Introduction"),
                ] {
                    assert!(
                        text.iter()
                            .any(|(kind, value)| *kind == expected.0 && value == expected.1),
                        "{text:?}"
                    );
                }
                assert!(
                    text.iter()
                        .any(|(kind, value)| *kind == TextBlockKind::Paragraph
                            && value.starts_with("This chapter provides")
                            && !value.starts_with("2.1"))
                );
            }
            if index == 90 || index == 91 {
                assert!(stats.removed_chrome_glyphs > 0);
                assert!(
                    !text
                        .iter()
                        .any(|(_, value)| value.starts_with("22 Chapter 2")
                            || value.starts_with("2.2 Computer Hardware 23")),
                    "{text:?}"
                );
            }
            if index == 95 {
                assert!(
                    !text
                        .iter()
                        .any(|(_, value)| value == "2.3 Display Technologies 27"),
                    "{text:?}"
                );
            }
            if index == 117 {
                assert!(
                    !text.iter().any(|(_, value)| value.contains("Brief Overview") && value.ends_with("49")),
                    "diagram-page running header leaked: {text:?}"
                );
            }
        }
        eprintln!(
            "Verified chapter headings and removed {} running-header glyphs on 38 local pages",
            stats.removed_chrome_glyphs
        );
    }

    fn plain(block: &TextBlock) -> String {
        block
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn outline_recovers_wrapped_title_and_large_margin_ordinal_once() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        for (text, y, size) in [
            ("CHAPTER 2", 100.0, 24.0),
            ("History of Desktop", 160.0, 29.0),
            ("Devices", 192.0, 29.0),
            ("Introduction", 230.0, 12.0),
        ] {
            row(text, 50.0, y, size, &mut page.glyphs);
        }
        row("2.1 ", 50.0, 245.0, 24.0, &mut page.glyphs);
        row(
            "This chapter provides a brief history.",
            105.0,
            245.0,
            10.0,
            &mut page.glyphs,
        );
        mark_outline_headings(
            &mut page,
            &[
                ("2 History of Desktop Devices".into(), 2),
                ("2.1 Introduction".into(), 3),
            ],
        );
        assert_eq!(page.headings.len(), 2);
        let (stored, _) =
            build(&page, 0, 10.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        let text = stored
            .section
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(t) => Some((t.kind, plain(t))),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            text,
            [
                (TextBlockKind::HeadingOrdinal(2), "CHAPTER 2".into()),
                (
                    TextBlockKind::Heading(2),
                    "History of Desktop Devices".into()
                ),
                (TextBlockKind::HeadingOrdinal(3), "2.1".into()),
                (TextBlockKind::Heading(3), "Introduction".into()),
                (
                    TextBlockKind::Paragraph,
                    "This chapter provides a brief history.".into()
                )
            ]
        );
        assert_eq!(stored.provenance.len(), page.glyphs.len());
        for b in &stored.section.blocks {
            if let Block::Text(t) = b
                && t.kind.is_heading()
            {
                assert!(t.content.iter().all(|i| matches!(i, Inline::Text(run) if run.style.size_scale == 1.0 && run.style.baseline == TextBaseline::Normal)));
            }
        }
    }

    #[test]
    fn ordinary_numeric_body_prefix_is_not_moved_into_heading() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        row("Introduction", 50.0, 100.0, 12.0, &mut page.glyphs);
        row(
            "2.1 reasons for this example.",
            50.0,
            115.0,
            10.0,
            &mut page.glyphs,
        );
        mark_outline_headings(&mut page, &[("2.1 Introduction".into(), 2)]);
        assert_eq!(page.headings.len(), 1);
        assert!(page.headings[0].ordinal.is_empty());
        let (stored, _) =
            build(&page, 0, 10.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert!(stored.section.blocks.iter().any(|b| matches!(b, Block::Text(t) if t.kind == TextBlockKind::Paragraph && plain(t).starts_with("2.1 reasons"))));
    }

    #[test]
    fn edge_detection_preserves_large_titles_and_dense_body_rows() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        row("9 Chapter 2 Computing", 50.0, 105.0, 9.0, &mut page.glyphs);
        row(
            "Body starts below the header.",
            50.0,
            145.0,
            10.0,
            &mut page.glyphs,
        );
        row(
            "And continues on the next row.",
            50.0,
            159.0,
            10.0,
            &mut page.glyphs,
        );
        let signature = edge_signatures(&page);
        assert_eq!(signature.len(), 1);
        let key = signature.iter().next().unwrap().clone();
        page.glyphs.clear();
        row(
            "100 Chapter 2 Computing",
            50.0,
            105.0,
            9.0,
            &mut page.glyphs,
        );
        row(
            "Body starts below the header.",
            50.0,
            145.0,
            10.0,
            &mut page.glyphs,
        );
        assert!(edge_signatures(&page).contains(&key));
        page.glyphs.clear();
        row("Chapter 2 Computing", 50.0, 105.0, 20.0, &mut page.glyphs);
        row(
            "Body starts below the heading.",
            50.0,
            145.0,
            10.0,
            &mut page.glyphs,
        );
        assert!(edge_signatures(&page).is_empty());
        page.glyphs.clear();
        row("Body at the page top", 50.0, 105.0, 10.0, &mut page.glyphs);
        row("continues here", 50.0, 119.0, 10.0, &mut page.glyphs);
        row("and here", 50.0, 133.0, 10.0, &mut page.glyphs);
        assert!(edge_signatures(&page).is_empty());
        assert!(repeats_near_page(
            &[90, 92, 94],
            91,
            "top:13:19:plain:chapter"
        ));
        assert!(!repeats_near_page(
            &[0, 100, 200],
            100,
            "top:13:19:plain:chapter"
        ));
        assert!(repeats_near_page(
            &[90, 92],
            91,
            "top:13:19:counter:chapter"
        ));
        assert!(!repeats_near_page(&[90, 92], 91, "top:13:19:plain:chapter"));
    }

    #[test]
    #[ignore = "Set TORTO_REFLOW_TEST_PDF to the local Pick, Click, Flick! PDF"]
    fn local_pdf_chapter_two_preserves_body_row_order() {
        let path = std::env::var_os("TORTO_REFLOW_TEST_PDF").expect("local PDF path");
        let bytes = fs::read(path).unwrap();
        let publication = crate::pdf::open(bytes, "local-probe.pdf").unwrap();
        let tags = extract::structure_tags(&publication.pdf);
        let page = extract::page(&publication.pdf, 89, &tags);
        let start = std::time::Instant::now();
        let grouped = lines(page.glyphs.clone());
        let normalized = grouped
            .iter()
            .map(|l| l.text().split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>();
        for expected in [
            "This chapter provides a brief history of “regular” computers to form a basis for",
            "understanding their interaction techniques. Since we are going back in history, this",
            "This section summarizes the key aspects of the hardware for computers as it relates",
        ] {
            assert!(
                normalized.iter().any(|line| line.contains(expected)),
                "Missing expected row: {expected}; rows: {normalized:?}"
            );
        }
        assert_eq!(
            grouped.iter().map(|l| l.glyphs.len()).sum::<usize>(),
            page.glyphs.len()
        );
        eprintln!(
            "Grouped {} glyphs into {} lines in {:?}",
            page.glyphs.len(),
            grouped.len(),
            start.elapsed()
        );
        let (stored, _) =
            build(&page, 89, 10.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert_eq!(stored.provenance.len(), page.glyphs.len());
        let plain = stored
            .section
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(t) => Some(
                    t.content
                        .iter()
                        .filter_map(|i| match i {
                            Inline::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>();
        assert!(
            plain.iter().any(|s| s.contains("This chapter provides")
                && s.contains("understanding their interaction techniques")),
            "{plain:?}"
        );
    }

    #[test]
    fn ruled_cells_own_text_once_and_frames_are_not_tables() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        for y in [100.0, 140.0, 180.0] {
            page.rules.push([40.0, y, 500.0, y]);
        }
        for x in [40.0, 240.0, 500.0] {
            page.rules.push([x, 100.0, x, 180.0]);
        }
        for y in [125.0, 165.0] {
            for x in [50.0, 250.0] {
                page.glyphs
                    .push(glyph("cell", x, y, 12.0, page.glyphs.len()));
            }
        }
        let mut stats = Statistics::default();
        let (stored, _) = build(&page, 0, 12.0, &HashSet::new(), &mut stats).unwrap();
        assert_eq!(stats.tables, 1);
        assert_eq!(stored.provenance.len(), 4);
        let Block::Table(table) = &stored.section.blocks[0] else {
            panic!()
        };
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 2);
        page.rules.retain(|r| r[0] != 240.0 && r[1] != 140.0);
        let (stored, _) =
            build(&page, 0, 12.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert!(
            !stored
                .section
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Table(_)))
        );
        assert_eq!(stored.provenance.len(), 4);
    }

    #[test]
    fn matched_superscripts_link_to_native_notes_and_unmatched_print_stays_visible() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        page.glyphs.push(glyph("Body", 50.0, 100.0, 12.0, 0));
        page.glyphs.push(glyph("1", 58.0, 97.0, 7.0, 1));
        page.glyphs.push(glyph("1 A footnote", 50.0, 740.0, 9.0, 2));
        let mut stats = Statistics::default();
        let (stored, _) = build(&page, 0, 12.0, &HashSet::new(), &mut stats).unwrap();
        assert_eq!(stats.notes, 1);
        assert_eq!(stored.provenance.len(), 3);
        assert!(
            stored
                .section
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Note(_)))
        );
        let Block::Text(body) = &stored.section.blocks[0] else {
            panic!()
        };
        assert!(body.content.iter().any(|i|matches!(i,Inline::Text(t) if t.style.link_role==LinkRole::FootnoteReference&&t.link.is_some())));
    }

    #[test]
    fn repeated_edge_text_requires_both_position_and_repetition() {
        let page = NativePage {
            width: 600.0,
            height: 800.0,
            glyphs: vec![
                glyph("RUNNING HEADER", 50.0, 50.0, 12.0, 0),
                glyph("Body one", 50.0, 300.0, 12.0, 1),
                glyph("Body two", 50.0, 315.0, 12.0, 2),
            ],
            ..NativePage::default()
        };
        let keys = edge_signatures(&page);
        assert_eq!(keys.len(), 1);
        let mut stats = Statistics::default();
        let (stored, _) = build(&page, 0, 12.0, &keys, &mut stats).unwrap();
        assert_eq!(stats.removed_chrome_glyphs, 1);
        assert_eq!(stored.provenance.len(), 2);
    }
    #[test]
    fn columns_do_not_interleave_and_provenance_is_unique() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        for row in 0..6 {
            for (x, text) in [(50.0, "left"), (350.0, "right")] {
                page.glyphs.push(glyph(
                    text,
                    x,
                    100.0 + f64::from(row) * 15.0,
                    12.0,
                    page.glyphs.len(),
                ));
            }
        }
        let (stored, _) =
            build(&page, 0, 12.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        let text = stored
            .section
            .blocks
            .iter()
            .filter_map(|b| {
                if let Block::Text(t) = b {
                    Some(
                        t.content
                            .iter()
                            .filter_map(|i| {
                                if let Inline::Text(t) = i {
                                    Some(t.text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<String>(),
                    )
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(text.len(), 2);
        assert!(text[0].contains("left") && !text[0].contains("right"));
        assert!(text[1].contains("right"));
        assert_eq!(stored.provenance.len(), 12);
    }
    #[test]
    fn unmappable_pages_preserve_original_instead_of_partial_text() {
        let page = NativePage {
            width: 600.0,
            height: 800.0,
            glyphs: vec![glyph("abc", 50.0, 100.0, 12.0, 0)],
            unmapped: 20,
            ..NativePage::default()
        };
        let (stored, crops) =
            build(&page, 0, 12.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        assert!(matches!(&stored.section.blocks[0], Block::Image(_)));
        assert_eq!(crops.len(), 1);
    }
    #[test]
    fn body_bold_is_not_a_heading_and_cjk_lines_do_not_gain_spaces() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..NativePage::default()
        };
        for (i, t) in ["中文第一行", "中文第二行", "中文第三行"]
            .iter()
            .enumerate()
        {
            let mut g = glyph(t, 50.0, 100.0 + i as f64 * 15.0, 12.0, i);
            g.bold = true;
            page.glyphs.push(g);
        }
        let (stored, _) =
            build(&page, 0, 12.0, &HashSet::new(), &mut Statistics::default()).unwrap();
        let Block::Text(text) = &stored.section.blocks[0] else {
            panic!()
        };
        assert_eq!(text.kind, TextBlockKind::Paragraph);
        assert_eq!(
            text.content
                .iter()
                .filter_map(|i| if let Inline::Text(t) = i {
                    Some(t.text.as_str())
                } else {
                    None
                })
                .collect::<String>(),
            "中文第一行中文第二行中文第三行"
        );
    }
}
