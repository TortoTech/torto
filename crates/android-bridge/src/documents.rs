//! On-demand native note and figure/table popup pages. No book progress writes.
use crate::*;
use rebook_publication::Section;
use rebook_renderer::PageDisplayList;

pub(crate) struct Document {
    id: String,
    note: Option<Note>,
    target: Option<rebook_app_core::media::Target>,
    selection: Option<(
        u32,
        rebook_reader::ReaderTextHit,
        rebook_reader::ReaderTextHit,
    )>,
    source: Arc<dyn BookSource>,
    section: Section,
    style: ReaderStyle,
    viewport: Option<(u32, u32, u32, u32, u32)>,
    pages: Vec<Arc<PageDisplayList>>,
    origin: Option<SourceRange>,
    session: Option<ReaderSession>,
    positions: Vec<rebook_reader::ReaderPosition>,
}
#[derive(Clone)]
struct Note {
    markup: String,
    target: String,
    marker: String,
    token: u64,
    indent: f32,
}

fn inline_target(target:&str)->Option<(usize,u64,u32)> {
    let mut parts=target.strip_prefix("inline-note:")?.split(':');
    let result=(parts.next()?.parse().ok()?,parts.next()?.parse().ok()?,parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(result)
}
/// A popup contributes its own visible ranges, never the entire endnote chapter.
pub(crate) fn translation_target(s: &ReaderState) -> Option<services::requests::Target> {
    let document = s.document.as_ref()?;
    let (index, ranges) = if let Some(note) = &document.note {
        if let Some((index,owner,_))=inline_target(&note.target) {
            let section=s.pipeline.rewrite.parse_section(index).ok()?;
            let ranges=section.blocks.iter().flat_map(|b|b.text_blocks()).filter_map(|t|t.source.clone()).filter(|r|rebook_publication::source_block_identity(r)==owner).collect();
            return Some(services::requests::Target {index,ranges,prefetch:false,defer:false});
        }
        let target = PublicationUrl::parse(&note.target).ok()?;
        let index = s.source.book().sections.iter().position(|p| p.href.path() == target.path())?;
        let section = s.pipeline.rewrite.parse_section(index).ok()?;
        (index, rebook_app_core::reading_text::focus_footnote_translation_ranges_in_section(&section, &target))
    } else {
        (document.target.as_ref()?.section, document.section.blocks.iter().flat_map(rebook_app_core::plugins::semantic_layout::block_ranges).collect())
    };
    (!ranges.is_empty()).then_some(services::requests::Target {index, ranges, prefetch:false, defer:false})
}

/// Refresh only popup pixels. Body reflow remains dirty until the popup closes.
pub(crate) fn refresh(s: &mut ReaderState) -> Result<bool> {
    let Some(document) = &s.document else { return Ok(false); };
    if let Some(spec) = document.note.clone() {
        let markup = live_note(s, &spec);
        if markup == spec.markup { return Ok(false); }
        note(s, &markup, &spec.target, &spec.marker, spec.token, spec.indent)?;
        Ok(true)
    } else if let Some(target) = document.target.clone() {
        let id = document.id.clone();
        reference(s, target)?;
        s.document.as_mut().unwrap().id = id;
        Ok(true)
    } else { Ok(false) }
}
fn live_note(s: &ReaderState, spec: &Note) -> String {
    if let Some((index,owner,number))=inline_target(&spec.target) {
        let markup=s.source.parse_section(index).ok().map(|section|section.blocks.iter()
            .filter(|block|block.text_blocks().filter_map(|t|t.source.as_ref()).any(|r|rebook_publication::source_block_identity(r)==owner))
            .map(|block|interactions::inline_note_markup(block,number)).filter(|s|!s.is_empty()).collect::<Vec<_>>().join("\n\n")).unwrap_or_default();
        return if markup.is_empty(){spec.markup.clone()}else{markup};
    }
    PublicationUrl::parse(&spec.target).ok().filter(|t| t.fragment().is_some())
        .map(|target| crate::interactions::rich_note(s, &target, &spec.marker))
        .filter(|markup| !markup.is_empty()).unwrap_or_else(|| spec.markup.clone())
}

#[derive(uniffi::Record)]
pub struct DocumentPage {
    pub id: String,
    pub index: u32,
    pub total: u32,
    /// Total measured height of all slices, without decoding their bitmaps.
    pub content_height: f32,
    pub controls: Vec<ReadingControl>,
    pub first_text_baseline: Option<f32>,
    pub page: Arc<MobilePage>,
}

pub(crate) fn reference(
    s: &mut ReaderState,
    target: rebook_app_core::media::Target,
) -> Result<interactions::TouchAction> {
    let mut section = s.source.parse_section(target.section).map_err(error)?;
    section.blocks = s.media.preview(&target).map_err(error)?;
    if serde_json::to_vec(&section).map_err(error)?.len() > 2 * 1024 * 1024 {
        return Err(error(
            "Preview exceeds size limit; open the original location",
        ));
    }
    let source = rebook_app_core::reading::FragmentSource::new(s.source.clone(), section.clone());
    let id = uuid_token();
    let title = format!(
        "{} {}",
        match target.key.kind {
            rebook_publication::media_references::MediaKind::Figure => "Figure",
            rebook_publication::media_references::MediaKind::Table => "Table",
            _ => "Equation",
        },
        target.key.number
    );
    s.document = Some(Document {
        selection: None,
        note: None,
        target: Some(target),
        id: id.clone(),
        source,
        section,
        style: s.session.style(),
        viewport: None,
        pages: vec![],
        session: None,
        positions: vec![],
        origin: None,
    });
    Ok(interactions::TouchAction::new("reference", title, id))
}
fn uuid_token() -> String {
    format!("document-{}", scroll::context_id())
}
fn note(s: &mut ReaderState, markup: &str, target: &str, marker: &str, token: u64, indent: f32) -> Result<()> {
    if markup.len() > 128 * 1024 {
        return Err(error("Note exceeds preview size limit"));
    }
    let spec = Note { markup: markup.into(), target: target.into(), marker: marker.into(), token, indent };
    let markup = live_note(s, &spec);
    let id = format!("note-{token}-{}-{}", blake3::hash(markup.as_bytes()), indent.to_bits());
    if s.document
        .as_ref()
        .is_some_and(|d| d.id == id && d.style == s.session.style())
    {
        return Ok(());
    }
    let mut descriptor = s.source.book().sections[s.session.location().section_index].clone();
    descriptor.id = rebook_publication::SpineItemId::new("popup-note").map_err(error)?;
    let mut section = rebook_app_core::reading::note_section(&markup, &descriptor).map_err(error)?;
    // Reserve the measured marker only on the first text line. The marker is
    // painted by the host, so copying/selecting body text never includes it.
    if let Some(first) = section.blocks.iter_mut().flat_map(|b| b.text_blocks_mut()).next() {
        first.style.indent = indent.clamp(0., 160.);
    }
    let origin = s.focus_anchor.clone().or_else(|| {
        s.session
            .current_visible_content_sources()
            .ok()?
            .into_iter()
            .flat_map(|(_, r)| r)
            .next()
    });
    let source = rebook_app_core::reading::FragmentSource::with_origin(
        s.source.clone(),
        section.clone(),
        origin.as_ref(),
    );
    s.document = Some(Document {
        selection: None,
        note: Some(Note { markup, ..spec }),
        target: None,
        id,
        source,
        section,
        style: s.session.style(),
        viewport: None,
        pages: vec![],
        session: None,
        positions: vec![],
        origin,
    });
    Ok(())
}
fn page(
    s: &mut ReaderState,
    id: &str,
    index: u32,
    viewport: Viewport,
    foreground: u32,
    background: u32,
) -> Result<DocumentPage> {
    let mut logical = viewport.validate()?;
    // A sheet's visible height is a scroll window, not a new image fit area.
    // Paginate references with the body's height so tall images retain the
    // same 80% height cap; the host scrolls the resulting native slices.
    if id.starts_with("document-") {
        logical = Viewport { height: s.session.viewport().height, ..viewport.clone() }.validate()?;
    }
    let document = s
        .document
        .as_mut()
        .filter(|d| d.id == id)
        .ok_or_else(|| error("Preview is no longer open"))?;
    let key = (
        viewport.width,
        logical.height,
        viewport.scale.to_bits(),
        foreground,
        background,
    );
    if document.viewport != Some(key) {
        let mut style = if id.starts_with("document-") {
            // Keep the body's full typography, including its smaller caption
            // font floor and actual Book/Unified mode. The sheet already owns
            // the same 20dp horizontal inset as the body.
            let mut style = document.style.clone();
            style.horizontal_margin = 0.0;
            style.minimum_horizontal_margin = 0.0;
            style.minimum_content_width = 0.0;
            style.spread = rebook_layout::SpreadMode::Single;
            style
        } else {
            rebook_app_core::reading::note_style(
                &document.style,
                (document.style.typography.font_size*0.8).max(14.0),
            )
        };
        let color = |argb: u32| rebook_publication::Rgba {
            red: (argb >> 16) as u8,
            green: (argb >> 8) as u8,
            blue: argb as u8,
            alpha: (argb >> 24) as u8,
        };
        style.foreground = color(foreground);
        style.background = color(background);
        style.writing_system = document.source.book().metadata.writing_system();
        let mut session = ReaderSession::open_deferred_with_fonts(
            document.source.clone(),
            logical,
            style,
            rebook_app_core::fonts::reader_fonts(),
            None,
        )
        .map_err(error)?;
        session.set_cache_policy(rebook_reader::ReaderCachePolicy {
            layout_bytes: 4 * 1024 * 1024,
            raster_bytes: 8 * 1024 * 1024,
            prefetch_distance: 0,
        });
        let pages = session.current_reading_unit_pages().map_err(error)?;
        if pages.len() > 512 {
            return Err(error(
                "Preview has too many pages; open the original location",
            ));
        }
        document.positions = pages.iter().map(|p| p.position).collect();
        document.pages = pages.into_iter().map(|p| p.page).collect();
        document.session = Some(session);
        document.viewport = Some(key);
    }
    let display = document
        .pages
        .get(index as usize)
        .ok_or_else(|| error("Preview page does not exist"))?;
    for (request, _) in display.deferred_rasters() {
        request
            .load_guarded(document.source.as_ref(), &|| true)
            .map_err(error)?;
    }
    let height = (display.content_bottom().unwrap_or(1.0) + 4.0)
        .ceil()
        .clamp(1.0, logical.height as f32) as f32;
    let width = (viewport.width as f32 * viewport.scale).ceil() as u32;
    let pixels_height = (height * viewport.scale).ceil() as u32;
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, pixels_height);
    let mut pixels = Vec::new();
    renderer.render_to_vec(
        |scene| {display.paint_scaled(scene, viewport.scale);
            let [r,g,b,a]=rebook_renderer::reference_color(background&0x00ff_0000<0x0080_0000);
            display.paint_footnote_icons_scaled(scene,peniko::Color::from_rgba8(r,g,b,a),viewport.scale);
        },
        &mut pixels,
    );
    let position = document.positions[index as usize];
    let regions = document
        .session
        .as_ref()
        .unwrap()
        .cached_visible_text_fragments_for_pages(&[position])
        .into_iter()
        .map(|f| TextRegion {
            bounds: display
                .source_rects(std::slice::from_ref(&f.range))
                .iter()
                .map(|r| Rect {
                    x: r.x0 as f32,
                    y: r.y0 as f32,
                    width: r.width() as f32,
                    height: r.height() as f32,
                })
                .collect(),
            text: f.text,
            source: serde_json::to_string(&f.range).unwrap_or_default(),
        })
        .collect();
    let total = document.pages.len() as u32;
    let content_height = document.pages.iter().map(|page| {
        let height = (page.content_bottom().unwrap_or(1.0) + 4.0).ceil().clamp(1.0, logical.height as f32);
        (height * viewport.scale).ceil() / viewport.scale
    }).sum();
    let ranges = document
        .section
        .blocks
        .iter()
        .flat_map(|b| {
            rebook_app_core::reading_text::block_source_range(b)
                .cloned()
                .into_iter()
                .chain(b.text_blocks().filter_map(|t| t.source.clone()))
        })
        .collect::<Vec<_>>();
    let first_text_baseline = document.section.blocks.iter().flat_map(|b|b.text_blocks()).filter_map(|t|t.source.as_ref()).find_map(|source|display.source_text_baseline(source));
    let controls = if document.target.is_some() {
        presentation::for_ranges(s, &ranges)?
    } else {
        vec![]
    };
    Ok(DocumentPage {
        id: id.into(),
        index,
        total,
        content_height,
        controls,
        first_text_baseline,
        page: MobilePage::new(
            width,
            pixels_height,
            pixels,
            PageInfo {
                regions,
                ..Default::default()
            },
        ),
    })
}
#[uniffi::export]
impl MobileReader {
    pub async fn prepare_media_references(self: Arc<Self>) -> Result<bool> {
        work(move || {
            let source = self.with(|s| Ok(s.media.clone()))?;
            source.prepare().map_err(error)
        })
        .await
    }
    pub async fn refresh_media_references(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                reflow::refresh(s)?;
                render(s)
            })
        })
        .await
    }
    pub async fn note_page(
        self: Arc<Self>,
        markup: String,
        token: u64,
        target: String,
        marker: String,
        first_line_indent: f32,
        index: u32,
        viewport: Viewport,
        foreground: u32,
        background: u32,
    ) -> Result<DocumentPage> {
        work(move || {
            self.with(|s| {
                note(s, &markup, &target, &marker, token, first_line_indent)?;
                let id = s.document.as_ref().unwrap().id.clone();
                page(s, &id, index, viewport, foreground, background)
            })
        })
        .await
    }
    pub async fn reference_page(
        self: Arc<Self>,
        id: String,
        index: u32,
        viewport: Viewport,
        foreground: u32,
        background: u32,
    ) -> Result<DocumentPage> {
        work(move || self.with(|s| page(s, &id, index, viewport, foreground, background))).await
    }
    pub async fn toggle_document_control(
        self: Arc<Self>,
        id: String,
        control: ReadingControl,
    ) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                let target = s
                    .document
                    .as_ref()
                    .filter(|d| d.id == id)
                    .and_then(|d| d.target.clone())
                    .ok_or_else(|| error("Preview is no longer open"))?;
                presentation::toggle(s, &control)?;
                reference(s, target)?;
                s.document.as_mut().unwrap().id = id;
                reflow::refresh(s)?;
                render(s)
            })
        })
        .await
    }
    pub async fn close_document(self: Arc<Self>, id: String) -> Result<()> {
        work(move || {
            self.with(|s| {
                if s.document.as_ref().is_some_and(|d| d.id == id) {
                    s.document = None;
                }
                Ok(())
            })
        })
        .await
    }
    pub async fn document_select(
        self: Arc<Self>,
        id: String,
        index: u32,
        x: f32,
        y: f32,
        end_x: f32,
        end_y: f32,
        handle: Option<bool>,
    ) -> Result<Option<SelectedText>> {
        work(move || {
            self.with(|s| {
                if ![x, y, end_x, end_y].iter().all(|v| v.is_finite()) {
                    return Err(error("Invalid selection coordinates"));
                }
                let d = s
                    .document
                    .as_mut()
                    .filter(|d| d.id == id)
                    .ok_or_else(|| error("Preview is no longer open"))?;
                let pos = *d
                    .positions
                    .get(index as usize)
                    .ok_or_else(|| error("Preview page does not exist"))?;
                let session = d
                    .session
                    .as_mut()
                    .ok_or_else(|| error("Preview is not ready"))?;
                session.set_visible_position(pos).map_err(error)?;
                let Some(hit) = session
                    .hit_test_current_spread(end_x, end_y, false)
                    .map_err(error)?
                else {
                    return Ok(None);
                };
                let (mut a, mut b) = if let Some(handle) = handle {
                    let Some((selected_index, start, end)) =
                        d.selection.as_ref().filter(|(i, _, _)| *i == index)
                    else {
                        return Ok(None);
                    };
                    let _ = selected_index;
                    if handle {
                        (hit, end.clone())
                    } else {
                        (start.clone(), hit)
                    }
                } else {
                    let Some(anchor) = session
                        .hit_test_current_spread(x, y, false)
                        .map_err(error)?
                    else {
                        return Ok(None);
                    };
                    (anchor, hit)
                };
                if a.ordering_key() > b.ordering_key() {
                    std::mem::swap(&mut a, &mut b)
                }
                d.selection = Some((index, a.clone(), b.clone()));
                Ok(session
                    .selection_between_with_granularity(&a, &b, SelectionGranularity::Word)
                    .map_err(error)?
                    .map(|selection| SelectedText {
                        highlighted: false,
                        text: selection.text,
                        bounds: selection
                            .rects
                            .iter()
                            .map(|r| Rect {
                                x: r.x,
                                y: r.y,
                                width: r.width,
                                height: r.height,
                            })
                            .collect(),
                    }))
            })
        })
        .await
    }
    pub async fn document_activate(
        self: Arc<Self>,
        id: String,
        index: u32,
        x: f32,
        y: f32,
    ) -> Result<interactions::TouchAction> {
        work(move || {
            self.with(|s| {
                let d = s
                    .document
                    .as_ref()
                    .filter(|d| d.id == id)
                    .ok_or_else(|| error("Preview is no longer open"))?;
                let p = d
                    .pages
                    .get(index as usize)
                    .ok_or_else(|| error("Preview page does not exist"))?;
                if let Some(hit) = p.media_reference_at(x, y) {
                    if let Some(target) = s
                        .media
                        .resolve(d.origin.as_ref().unwrap_or(&hit.source), &hit.key)
                    {
                        return reference(s, target);
                    }
                }
                let Some(hit) = p.hit_test_text(x, y, false) else {
                    return Ok(interactions::TouchAction::new(
                        "none",
                        String::new(),
                        String::new(),
                    ));
                };
                let Some(range) = p
                    .text_region_source_range(hit.region_index, hit.cluster_start..hit.cluster_end)
                else {
                    return Ok(interactions::TouchAction::new(
                        "none",
                        String::new(),
                        String::new(),
                    ));
                };
                if let Some((target, _, _)) = interactions::linked(&d.section, &range.start) {
                    return Ok(interactions::TouchAction::new(
                        if target.website_url().is_some() {
                            "external"
                        } else {
                            "link"
                        },
                        String::new(),
                        target.to_string(),
                    ));
                }
                Ok(interactions::TouchAction::new(
                    "none",
                    String::new(),
                    String::new(),
                ))
            })
        })
        .await
    }
}

#[cfg(test)]
pub(crate) mod tests;
