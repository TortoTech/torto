//! On-demand native note and figure/table popup pages. No book progress writes.
use crate::*;
use rebook_publication::Section;
use rebook_renderer::PageDisplayList;

pub(crate) struct Document {
    id: String,
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
#[derive(uniffi::Record)]
pub struct DocumentPage {
    pub id: String,
    pub index: u32,
    pub total: u32,
    pub controls: Vec<ReadingControl>,
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
fn note(s: &mut ReaderState, markup: &str, token: u64) -> Result<()> {
    if markup.len() > 128 * 1024 {
        return Err(error("Note exceeds preview size limit"));
    }
    let id = format!("note-{token}-{}", blake3::hash(markup.as_bytes()));
    if s.document
        .as_ref()
        .is_some_and(|d| d.id == id && d.style == s.session.style())
    {
        return Ok(());
    }
    let mut descriptor = s.source.book().sections[s.session.location().section_index].clone();
    descriptor.id = rebook_publication::SpineItemId::new("popup-note").map_err(error)?;
    let section = rebook_app_core::reading::note_section(markup, &descriptor).map_err(error)?;
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
    let logical = viewport.validate()?;
    let document = s
        .document
        .as_mut()
        .filter(|d| d.id == id)
        .ok_or_else(|| error("Preview is no longer open"))?;
    let key = (
        viewport.width,
        viewport.height,
        viewport.scale.to_bits(),
        foreground,
        background,
    );
    if document.viewport != Some(key) {
        let mut style = rebook_app_core::reading::note_style(
            &document.style,
            document.style.typography.font_size,
        );
        if id.starts_with("document-") {
            style.typesetting =
                rebook_app_core::reading::typesetting(&style.typesetting, TypesettingMode::Unified);
            style.top_margin = 8.0;
            style.bottom_margin = 8.0;
        }
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
        .clamp(1.0, viewport.height as f32) as f32;
    let width = (viewport.width as f32 * viewport.scale).ceil() as u32;
    let pixels_height = (height * viewport.scale).ceil() as u32;
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, pixels_height);
    let mut pixels = Vec::new();
    renderer.render_to_vec(
        |scene| display.paint_scaled(scene, viewport.scale),
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
    let controls = if document.target.is_some() {
        presentation::for_ranges(s, &ranges)?
    } else {
        vec![]
    };
    Ok(DocumentPage {
        id: id.into(),
        index,
        total,
        controls,
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
                s.session.refresh_source().map_err(error)?;
                s.scroll_center = true;
                scroll::invalidate(s);
                render(s)
            })
        })
        .await
    }
    pub async fn note_page(
        self: Arc<Self>,
        markup: String,
        token: u64,
        index: u32,
        viewport: Viewport,
        foreground: u32,
        background: u32,
    ) -> Result<DocumentPage> {
        work(move || {
            self.with(|s| {
                note(s, &markup, token)?;
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
                s.session.refresh_source().map_err(error)?;
                scroll::invalidate(s);
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
                    .hit_test_current_spread(end_x, end_y, true)
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
