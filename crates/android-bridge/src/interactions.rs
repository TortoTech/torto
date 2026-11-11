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
    pub marker: String,
    pub indicator: Option<Rect>,
}
impl TouchAction {
    pub(crate) fn new(kind: &str, text: String, target: String) -> Self {
        Self {
            kind: kind.into(),
            text,
            markup: String::new(),
            target,
            media: None,
            marker:String::new(),indicator:None,
        }
    }
}
impl TouchAction {
    fn with_markup(mut self, markup: String) -> Self {
        self.markup = markup;
        self
    }
    fn with_reference(mut self, marker:String, indicator:Option<Rect>)->Self {
        self.marker=marker;self.indicator=indicator;self
    }
}
pub(crate) fn rich_note(s: &ReaderState, target: &PublicationUrl, marker: &str) -> String {
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
    let marker = if markup.starts_with(marker) {marker} else {marker.trim_matches(['[',']'])};
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
    pub lines: Vec<Rect>,
    pub headings: Vec<Rect>,
    pub leading: bool,
    pub group: Option<rebook_app_core::focus_content::Group>,
}
fn union_rects(rects: &[Rect]) -> Option<Rect> {
    rects.iter().cloned().reduce(|a,b| Rect {
        x:a.x.min(b.x), y:a.y.min(b.y),
        width:(a.x+a.width).max(b.x+b.width)-a.x.min(b.x),
        height:(a.y+a.height).max(b.y+b.height)-a.y.min(b.y),
    })
}
impl FocusUnit {
    pub(crate) fn navigation_bounds(&self) -> Option<Rect> {
        let mut bounds=self.bounds.clone();
        if self.leading { bounds.extend_from_slice(&self.headings); }
        union_rects(&bounds)
    }
    pub(crate) fn paint_bounds(&self) -> Vec<Rect> {
        let mut result=self.navigation_bounds().into_iter().collect::<Vec<_>>();
        if !self.leading { result.extend(union_rects(&self.headings)); }
        result
    }
    fn merge(&mut self, other:Self) {
        self.ranges.extend(other.ranges);self.bounds.extend(other.bounds);
        self.lines.extend(other.lines);self.headings.extend(other.headings);
    }
}
/// Keep tall lists/containers navigable by top-level parts, as on desktop.
/// Missing parts beyond the bounded raster are never mistaken for a short group.
pub(crate) fn arrange_focus_units(units: Vec<FocusUnit>, height:f32) -> Vec<FocusUnit> {
    let mut output=Vec::new();
    let mut units=units.into_iter().filter(|u|!u.bounds.is_empty()).peekable();
    while let Some(mut first)=units.next() {
        let Some(group)=first.group.clone() else {output.push(first);continue;};
        let mut parts=vec![first];
        while units.peek().is_some_and(|unit|unit.group.as_ref()==Some(&group)) {parts.push(units.next().unwrap());}
        let bounds=parts.iter().flat_map(|unit|unit.bounds.iter().cloned()).collect::<Vec<_>>();
        // Desktop group cards include the block's 6dp and card's 4dp padding.
        let fits=parts.len()==group.parts && union_rects(&bounds).is_some_and(|rect|rect.height+20.0 <= (height-32.0).max(1.0)+1.0);
        if fits {
            first=parts.remove(0);
            for part in parts {first.merge(part);}
            output.push(first);
        }else {output.extend(parts);}
    }
    output
}
pub(crate) fn focus_units(s: &mut ReaderState) -> Result<Vec<FocusUnit>> {
    if !s.focus { return Ok(vec![]); }
    let specs=focus_specs(s)?;
    Ok(arrange_focus_units(focus_units_on(&specs,s.session.current_page()),s.session.viewport().height as f32))
}
pub(crate) fn focus_specs(s:&mut ReaderState)->Result<Vec<rebook_app_core::focus_content::Unit>> {
    let blocks=s.session.current_reading_unit_content().map_err(error)?.into_iter()
        .flat_map(|(_,section)|section.blocks).collect::<Vec<_>>();
    let allowed=s.session.current_reading_unit_source_ranges().map_err(error)?;
    Ok(rebook_app_core::focus_content::units_in_ranges(&blocks,true,Some(&allowed)))
}
pub(crate) fn focus_units_on(specs:&[rebook_app_core::focus_content::Unit],display:&rebook_renderer::PageDisplayList)->Vec<FocusUnit> {
    fn rects(rects:impl IntoIterator<Item=kurbo::Rect>)->Vec<Rect> {rects.into_iter().map(|r|Rect {
        x:r.x0 as f32,y:r.y0 as f32,width:r.width() as f32,height:r.height() as f32,
    }).collect()}
    specs.iter().filter_map(|unit| {
        let bounds=rects(display.source_content_bounds(&unit.ranges));
        let headings=rects(display.source_content_bounds(&unit.headings));
        if bounds.is_empty()&&headings.is_empty(){return None;}
        let lines=rects(display.source_rects(&unit.ranges));
        Some(FocusUnit {ranges:unit.ranges.clone(),bounds,lines,headings,leading:unit.leading,group:unit.group.clone()})
    }).collect()
}
pub(crate) fn linked(
    section: &rebook_publication::Section,
    anchor: &SourceAnchor,
) -> Option<(PublicationUrl, LinkRole, String)> {
    rebook_publication::links::at_source(&section.blocks,anchor)
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
pub(crate) fn inline_note_markup(block: &Block, number: u32) -> String {
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
                if number & 0x2000_0000 != 0 && ordinal == number & 0x1fff_ffff {
                    found.push(text::note_inline_markdown(&text.content[range]));
                }
            }
        }
        let citation = text
            .content
            .iter()
            .filter(|i| number & 0x2000_0000 == 0 && matches!(i,Inline::Text(r) if r.style.inline_citation==number&0x1fff_ffff))
            .cloned()
            .collect::<Vec<_>>();
        if !citation.is_empty() {
            found.push(text::note_inline_markdown(&citation));
        }
    }
    found.join("\n\n")
}
fn note_at(s: &mut ReaderState, x: f32, y: f32) -> Result<Option<TouchAction>> {
    let exact = s.session.footnote_reference_at_current_spread(x, y).map_err(error)?.is_some();
    let (x,y) = if exact {(x,y)} else {
        // Exact authored links and images outrank a neighbouring enlarged note.
        if s.session.image_at_current_spread(x,y).map_err(error)?.is_some() {return Ok(None);}
        let page = s.session.current_page();
        if let Some(hit) = page.hit_test_text(x,y,true)
            && let Some(range) = page.text_region_source_range(hit.region_index,hit.cluster_start..hit.cluster_end)
            && let Ok(section) = s.source.parse_section(s.session.snapshot().location.section_index)
            && linked(&section, &range.start).is_some() {return Ok(None);}
        let Some(point) = s.session.current_page().footnote_touch_point(x,y,48.) else {return Ok(None);};
        point
    };
    let Some((position, (owner, number))) = s
        .session
        .footnote_reference_at_current_spread(x, y)
        .map_err(error)?
    else {
        return Ok(None);
    };
    let label=if number&0x2000_0000==0 {format!("[{}]",number&0x1fff_ffff)}else{(number&0x1fff_ffff).to_string()};
    let indicator=s.session.current_page().footnote_indicator_at(x,y).map(|[x,y,width,height]|Rect{x,y,width,height});
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
                        TouchAction::new("note", content, format!("inline-note:{}:{owner}:{number}",position.section_index))
                            .with_markup(inline_note_markup(block, number)).with_reference(label.clone(),indicator.clone()),
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
                            .with_markup(rich_note(s, &target, &marker)).with_reference(label.clone(),indicator.clone()),
                    ));
                }
                FocusFootnoteSource::Citation {
                    text: content,
                    number: n,
                    ..
                } if n == number & 0x1fff_ffff => {
                    return Ok(Some(
                        TouchAction::new("note", content, format!("inline-note:{}:{owner}:{number}",position.section_index))
                            .with_markup(inline_note_markup(block, number)).with_reference(label.clone(),indicator.clone()),
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
            ..Default::default()
        },
    )
}

pub(crate) fn follow(s: &mut ReaderState, target: &PublicationUrl) -> Result<Arc<MobilePage>> {
    s.scroll_center = true;
    scroll::invalidate(s);
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
                if let Some(hit)=s.session.media_reference_at_current_spread(x,y).map_err(error)? {
                    if let Some(target)=s.media.resolve(&hit.source,&hit.key){return documents::reference(s,target)}
                }
                if let Some(mut note) = note_at(s, x, y)? {
                    if let Some(bar)=&mut note.indicator {bar.y+=view_y-y;}
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
                        marker:String::new(),indicator:None,
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
                        s.store.wake().allow_progress(&s.book_id);
                        let page = follow(s, &target)?;
                        return Ok(TouchAction {
                            kind: "page".into(),
                            text: String::new(),
                            markup: String::new(),
                            target: String::new(),
                            media: Some(page),
                            marker:String::new(),indicator:None,
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
                        u.paint_bounds().iter().any(|r| {
                            view_x >= r.x
                                && view_x <= r.x + r.width
                                && view_y >= r.y
                                && view_y <= r.y + r.height
                        })
                    })
                {
                    if s.focus_anchor.as_ref().is_some_and(|anchor|s.focus_units[index].ranges.iter().any(|range|range==anchor)) {
                        return Ok(TouchAction::new("none",String::new(),String::new()));
                    }
                    s.store.wake().allow_progress(&s.book_id);
                    s.scroll_center = true;
                    s.focus_anchor = s.focus_units[index].ranges.first().cloned();
                    let page = render(s)?;
                    return Ok(TouchAction {
                        kind: "focus".into(),
                        text: String::new(),
                        markup: String::new(),
                        target: String::new(),
                        media: Some(page),
                        marker:String::new(),indicator:None,
                    });
                }
                Ok(TouchAction::new("none", String::new(), String::new()))
            })
        })
        .await
    }
    pub async fn focus_mode_available(self: Arc<Self>) -> Result<bool> {
        work(move || self.with(|s| Ok(focus_mode_allowed(s)))).await
    }
    pub async fn focus_mode(self: Arc<Self>, enabled: bool) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                let enabled = enabled && focus_mode_allowed(s);
                let mut anchor = s.focus_anchor.clone().or_else(|| s.session.current_locator().source);
                s.focus = enabled;
                s.scroll_center = true;
                scroll::invalidate(s);
                s.pipeline
                    .structure
                    .set_enabled(enabled)
                    .map_err(error)?;
                let mut style = s.session.style();
                apply_presentation_mode(s, &mut style)?;
                style.spread = if enabled {
                    SpreadMode::Scroll
                } else {
                    SpreadMode::Single
                };
                s.session.refresh_source_with_style(style).map_err(error)?;
                if let Some(range) = &anchor {
                    if s.session.go_to_source(&range.start).is_err() {
                        // RestoreLocator already falls back for obsolete bookmarks.
                        anchor=s.session.current_locator().source;
                    }
                }
                s.focus_anchor = anchor;
                render(s)
            })
        })
        .await
    }
    pub async fn follow_link(self: Arc<Self>, target: String) -> Result<Arc<MobilePage>> {
        work(move || self.with(|s| {s.store.wake().allow_progress(&s.book_id);follow(s, &PublicationUrl::parse(&target).map_err(error)?) }))
            .await
    }
    pub async fn go_back(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                if let Some(locator) = s.history.last().cloned() {
                    s.store.wake().allow_progress(&s.book_id);
                    s.scroll_center = true;
                    scroll::invalidate(s);
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

fn focus_mode_allowed(s: &ReaderState) -> bool {
    s.format != rebook_formats::BookFormat::Pdf
        || s.source.book().metadata.layout != rebook_publication::RenditionLayout::PrePaginated
}
