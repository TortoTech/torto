//! Source-owned touch interactions. Kotlin only displays the result.
use crate::*;
use rebook_app_core::reading_text::{self as text, FocusFootnoteSource};
use rebook_publication::{Block, Inline, LinkRole, SourceAnchor};

#[derive(Clone, uniffi::Record)]
pub struct TouchAction {
    pub kind: String,
    pub text: String,
    pub markup: String,
    pub target: String,
    pub media: Option<Arc<MobilePage>>,
}
impl TouchAction {
    fn new(kind: &str, text: String, target: String) -> Self {
        Self {
            kind: kind.into(),
            text,
            markup: String::new(),
            target,
            media: None,
        }
    }
}
impl TouchAction {
    fn with_markup(mut self, markup: String) -> Self {
        self.markup = markup;
        self
    }
}
fn rich_note(s: &ReaderState, target: &PublicationUrl, marker: &str) -> String {
    let markup = s
        .source
        .book()
        .sections
        .iter()
        .position(|p| p.href.path() == target.path())
        .and_then(|i| s.source.parse_section(i).ok())
        .and_then(|section| text::focus_footnote_markdown_in_section(&section, target))
        .unwrap_or_default();
    let marker = marker.trim();
    let explicit_marker = !marker.is_empty()
        && markup
            .strip_prefix(marker)
            .and_then(|s| s.chars().next())
            .is_some_and(|c| {
                c.is_whitespace() || matches!(c, '.' | '．' | '，' | '、' | ')' | '）' | ']' | '】')
            });
    if explicit_marker {
        text::normalize_resolved_footnote_text(&markup, marker).unwrap_or(markup)
    } else {
        markup
    }
}

#[derive(Clone)]
pub(crate) struct FocusUnit {
    pub ranges: Vec<SourceRange>,
    pub bounds: Vec<Rect>,
}
pub(crate) fn focus_units(s: &ReaderState) -> Result<Vec<FocusUnit>> {
    if !s.focus {
        return Ok(vec![]);
    }
    let section = s
        .source
        .parse_section(s.session.snapshot().location.section_index)
        .map_err(error)?;
    Ok(focus_units_on(&section, s.session.current_page()))
}
pub(crate) fn focus_units_on(
    section: &rebook_publication::Section,
    display: &rebook_renderer::PageDisplayList,
) -> Vec<FocusUnit> {
    let refs: Vec<_> = section.blocks.iter().collect();
    let scopes = rebook_layout::semantic_list_groups(&refs)
        .into_iter()
        .chain(rebook_layout::semantic_paragraph_groups(&refs))
        .collect::<Vec<_>>();
    let mut units = Vec::new();
    let mut index = 0;
    while index < section.blocks.len() {
        let end = scopes
            .iter()
            .filter(|range| range.start == index)
            .map(|r| r.end)
            .max()
            .unwrap_or(index + 1);
        let ranges = section.blocks[index..end]
            .iter()
            .filter(|b| !b.is_footnote_definition())
            .flat_map(|b| {
                text::block_source_range(b)
                    .map_or_else(Vec::new, |r| text::focus_block_paint_ranges(b, r))
            })
            .collect::<Vec<_>>();
        let bounds = display
            .source_block_bounds(&ranges)
            .into_iter()
            .map(|r| Rect {
                x: r.x0 as f32,
                y: r.y0 as f32,
                width: r.width() as f32,
                height: r.height() as f32,
            })
            .collect::<Vec<_>>();
        if !bounds.is_empty() {
            units.push(FocusUnit { ranges, bounds });
        }
        index = end;
    }
    units
}
fn linked(
    section: &rebook_publication::Section,
    anchor: &SourceAnchor,
) -> Option<(PublicationUrl, LinkRole, String)> {
    for block in section.blocks.iter().flat_map(Block::text_blocks) {
        let Some(range) = block.source.as_ref() else {
            continue;
        };
        if range.start.spine != anchor.spine || range.start.node != anchor.node {
            continue;
        }
        let mut offset = range.start.text_offset;
        for inline in &block.content {
            let length = match inline {
                Inline::Text(r) => r.text.chars().count(),
                Inline::Ruby(r) => r.base_text().chars().count(),
                Inline::Math(_) | Inline::Image(_) | Inline::Break => 1,
            } as u64;
            if anchor.text_offset >= offset && anchor.text_offset < offset + length {
                if let Inline::Text(r) = inline {
                    return r
                        .link
                        .clone()
                        .map(|link| (link, r.style.link_role, r.text.clone()));
                }
                return None;
            }
            offset += length;
        }
    }
    None
}
fn resolve_note(s: &ReaderState, target: &PublicationUrl, marker: &str) -> Option<String> {
    let index = s
        .source
        .book()
        .sections
        .iter()
        .position(|section| section.href.path() == target.path())?;
    let section = s.source.parse_section(index).ok()?;
    text::focus_footnote_text_in_section(&section, target, marker)
}
fn inline_note_markup(block: &Block, number: u32) -> String {
    let mut found = Vec::new();
    for text in block.text_blocks() {
        for (index, range) in rebook_layout::paragraph_footnotes(text)
            .into_iter()
            .enumerate()
        {
            if let Some(Inline::Text(first)) = text.content.get(range.start) {
                let ordinal = if first.style.footnote_number == 0 {
                    index as u32 + 1
                } else {
                    first.style.footnote_number
                };
                if ordinal == number & 0x1fff_ffff {
                    found.push(text::note_inline_markdown(&text.content[range]));
                }
            }
        }
        let citation = text
            .content
            .iter()
            .filter(|i| matches!(i,Inline::Text(r) if r.style.inline_citation==number&0x1fff_ffff))
            .cloned()
            .collect::<Vec<_>>();
        if !citation.is_empty() {
            found.push(text::note_inline_markdown(&citation));
        }
    }
    found.join("\n\n")
}
fn note_at(s: &mut ReaderState, x: f32, y: f32) -> Result<Option<TouchAction>> {
    let Some((position, (owner, number))) = s
        .session
        .footnote_reference_at_current_spread(x, y)
        .map_err(error)?
    else {
        return Ok(None);
    };
    let section = s
        .source
        .parse_section(position.section_index)
        .map_err(error)?;
    for block in &section.blocks {
        for note in text::block_focus_footnotes(block) {
            let owner_matches = block
                .text_blocks()
                .filter_map(|t| t.source.as_ref())
                .any(|r| rebook_publication::source_block_identity(r) == owner)
                || text::block_source_range(block)
                    .is_some_and(|r| rebook_publication::source_block_identity(r) == owner);
            if !owner_matches {
                continue;
            }
            match note {
                FocusFootnoteSource::Inline(content, n) if n == number & 0x1fff_ffff => {
                    return Ok(Some(
                        TouchAction::new("note", content, String::new())
                            .with_markup(inline_note_markup(block, number)),
                    ));
                }
                FocusFootnoteSource::Reference {
                    marker,
                    target,
                    number: n,
                } if n == number & 0x1fff_ffff => {
                    let content =
                        resolve_note(s, &target, &marker).unwrap_or_else(|| marker.clone());
                    return Ok(Some(
                        TouchAction::new("note", content, target.to_string())
                            .with_markup(rich_note(s, &target, &marker)),
                    ));
                }
                FocusFootnoteSource::Citation {
                    text: content,
                    number: n,
                    ..
                } if n == number & 0x1fff_ffff => {
                    return Ok(Some(
                        TouchAction::new("note", content, String::new())
                            .with_markup(inline_note_markup(block, number)),
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(None)
}
pub(crate) fn formula_page(
    latex: &str,
    font_size: f32,
    color: rebook_publication::Rgba,
    limit: u32,
) -> Result<Arc<MobilePage>> {
    if latex.len() > 65536 || !font_size.is_finite() || !(8.0..=144.0).contains(&font_size) {
        return Err(error("公式尺寸超出限制"));
    }
    let math = rebook_math::math::render_math(
        latex,
        font_size,
        &format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue),
        true,
    )
    .map_err(error)?;
    let width = math.width + 24.0;
    let height = math.ascent + math.descent + 24.0;
    if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}"><g transform="translate(12 {})">{}</g></svg>"#,
            math.ascent + 12.0,
            math.svg_fragment
        );
        let tree =
            resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).map_err(error)?;
        let scale = (limit as f32 / width).min(limit as f32 / height).min(2.0);
        let w = (width * scale).ceil().clamp(1.0, limit as f32) as u32;
        let h = (height * scale).ceil().clamp(1.0, limit as f32) as u32;
        let mut pixmap =
            resvg::tiny_skia::Pixmap::new(w, h).ok_or_else(|| error("无法创建公式预览"))?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        return Ok(preview_page(w, h, pixmap.take()));
    }

    Err(error("公式尺寸无效"))
}
pub(crate) fn image_page(
    s: &ReaderState,
    hit: rebook_reader::ReaderImage,
) -> Result<Arc<MobilePage>> {
    if let Some(latex) = &hit.formula {
        return formula_page(latex, 72.0, s.session.style().foreground, 2048);
    }
    let mut decoded = None;
    if hit.formula.is_none()
        && let Some(origin) = &hit.origin
    {
        if let Ok(resource) = s.source.resource(&origin.href) {
            let mut reader = image::ImageReader::new(std::io::Cursor::new(resource.bytes.as_ref()))
                .with_guessed_format()
                .map_err(error)?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(64 * 1024 * 1024);
            reader.limits(limits);
            if resource.bytes.len() <= 32 * 1024 * 1024 {
                decoded = reader.decode().ok();
            }
        }
    }
    if decoded.is_none() && hit.pixels.len() > 64 * 1024 * 1024 {
        return Err(error("图片超过预览内存限制"));
    }
    let image = decoded
        .or_else(|| {
            image::RgbaImage::from_raw(hit.width, hit.height, hit.pixels.to_vec())
                .map(image::DynamicImage::ImageRgba8)
        })
        .ok_or_else(|| error("无法读取图片"))?;
    let raster = image.thumbnail(2048, 2048).to_rgba8();
    let (width, height) = raster.dimensions();
    let mut pixels = raster.into_raw();
    // Android bitmaps expect premultiplied RGBA; source images use straight alpha.
    for pixel in pixels.chunks_exact_mut(4) {
        let a = pixel[3] as u16;
        for c in &mut pixel[..3] {
            *c = ((*c as u16 * a + 127) / 255) as u8;
        }
    }
    Ok(preview_page(width, height, pixels))
}
pub(crate) fn preview_page(width: u32, height: u32, pixels: Vec<u8>) -> Arc<MobilePage> {
    MobilePage::new(
        width,
        height,
        pixels,
        PageInfo {
            section: 0,
            section_count: 0,
            page: 0,
            page_count: 0,
            progress: 0.0,
            regions: vec![],
            marks: vec![],
            focus_bounds: vec![],
            focus_fade_color: 0,
            focus_highlight: false,
            is_scroll: false,
            scroll_distance: 0.0,
            focus_index: 0,
            focus_count: 0,
            can_go_back: false,
        },
    )
}

pub(crate) fn follow(s: &mut ReaderState, target: &PublicationUrl) -> Result<Arc<MobilePage>> {
    s.scroll_center = true;
    s.scroll_slices.clear();
    let locator = s.session.current_locator();
    s.session.go_to_href(target).map_err(error)?;
    s.history.push(locator);
    if s.history.len() > 32 {
        s.history.remove(0);
    }
    s.focus_anchor = None;
    render(s)
}
#[uniffi::export]
impl MobileReader {
    pub async fn activate(self: Arc<Self>, x: f32, y: f32) -> Result<TouchAction> {
        work(move || {
            self.with(|s| {
                if !x.is_finite() || !y.is_finite() {
                    return Err(error("Invalid touch coordinates"));
                }
                let (view_x, view_y) = (x, y);
                let (x, y) = scroll::map_touch(s, x, y)?;
                if let Some(url) = s.session.website_at_current_spread(x, y).map_err(error)? {
                    return Ok(TouchAction::new("external", String::new(), url));
                }
                if let Some(note) = note_at(s, x, y)? {
                    return Ok(note);
                }
                if let Some(hit) = s.session.image_at_current_spread(x, y).map_err(error)? {
                    let formula = hit.formula.clone();
                    let target = hit
                        .source
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(error)?
                        .unwrap_or_default();
                    let media = image_page(s, hit)?;
                    return Ok(TouchAction {
                        kind: if formula.is_some() {
                            "formula"
                        } else {
                            "image"
                        }
                        .into(),
                        text: formula.unwrap_or_default(),
                        markup: String::new(),
                        target,
                        media: Some(media),
                    });
                }
                let display = s.session.current_page();
                if let Some(hit) = display.hit_test_text(x, y, true)
                    && let Some(range) = display.text_region_source_range(
                        hit.region_index,
                        hit.cluster_start..hit.cluster_end,
                    )
                {
                    let section = s
                        .source
                        .parse_section(s.session.snapshot().location.section_index)
                        .map_err(error)?;
                    if let Some((target, role, marker)) = linked(&section, &range.start) {
                        if target.website_url().is_some() {
                            return Ok(TouchAction::new(
                                "external",
                                String::new(),
                                target.to_string(),
                            ));
                        }
                        if role == LinkRole::FootnoteReference {
                            if let Some(content) = resolve_note(s, &target, &marker) {
                                return Ok(TouchAction::new("note", content, target.to_string())
                                    .with_markup(rich_note(s, &target, &marker)));
                            }
                        }
                        let page = follow(s, &target)?;
                        return Ok(TouchAction {
                            kind: "page".into(),
                            text: String::new(),
                            markup: String::new(),
                            target: String::new(),
                            media: Some(page),
                        });
                    }
                    let notes = s.store.annotations_for_book(&s.book_id).map_err(error)?;
                    if let Some(note) = notes.iter().find(|a| {
                        a.ranges
                            .iter()
                            .any(|r| text::source_range_contains_anchor(r, &range.start))
                    }) {
                        return Ok(TouchAction::new(
                            "annotation",
                            note.quote.clone(),
                            note.id.clone(),
                        ));
                    }
                }
                if s.focus
                    && let Some(index) = s.focus_units.iter().position(|u| {
                        u.bounds.iter().any(|r| {
                            view_x >= r.x
                                && view_x <= r.x + r.width
                                && view_y >= r.y
                                && view_y <= r.y + r.height
                        })
                    })
                {
                    s.scroll_center = true;
                    s.focus_anchor = s.focus_units[index].ranges.first().cloned();
                    let page = render(s)?;
                    return Ok(TouchAction {
                        kind: "page".into(),
                        text: String::new(),
                        markup: String::new(),
                        target: String::new(),
                        media: Some(page),
                    });
                }
                Ok(TouchAction::new("none", String::new(), String::new()))
            })
        })
        .await
    }
    pub async fn focus_mode(self: Arc<Self>, enabled: bool) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                s.focus = enabled;
                s.scroll_center = true;
                s.scroll_slices.clear();
                s.pipeline
                    .structure
                    .set_all_active(enabled && s.options.sentence_split)
                    .map_err(error)?;
                let mut style = s.session.style();
                style.spread = if enabled && s.options.scroll_focus {
                    SpreadMode::Scroll
                } else {
                    SpreadMode::Single
                };
                s.session.refresh_source_with_style(style).map_err(error)?;
                s.focus_anchor = None;
                render(s)
            })
        })
        .await
    }
    pub async fn follow_link(self: Arc<Self>, target: String) -> Result<Arc<MobilePage>> {
        work(move || self.with(|s| follow(s, &PublicationUrl::parse(&target).map_err(error)?)))
            .await
    }
    pub async fn go_back(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                if let Some(locator) = s.history.last().cloned() {
                    s.scroll_center = true;
                    s.scroll_slices.clear();
                    s.session.restore_locator(&locator).map_err(error)?;
                    s.history.pop();
                    s.focus_anchor = None;
                }
                render(s)
            })
        })
        .await
    }
}
