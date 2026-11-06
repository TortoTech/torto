//! Original-PDF viewport: source-preserving transforms and one coalesced worker.
use super::*;
use egui::{Pos2, Rect, Vec2};
use kurbo::Affine;
use rebook_reader::fixed_viewport::FixedPageTransform;
use serde::{Deserialize, Serialize};
use std::sync::mpsc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
enum Mode {
    #[default]
    FitPage,
    FitWidth,
    Actual,
    Manual(f32),
}
#[derive(Clone, Debug, PartialEq)]
struct Region {
    page: usize,
    rect: [f32; 4],
    normalized: [f32; 4],
    pixels: [u32; 2],
}
struct Patch {
    page: usize,
    rect: kurbo::Rect,
    image: peniko::ImageData,
    background: peniko::Color,
}
struct Job {
    generation: u64,
    source: Arc<dyn BookSource>,
    regions: Vec<Region>,
    mode: Mode,
    background: Rgba,
    ctx: egui::Context,
}
enum Ready {
    Loaded(Mode),
    Rendered(u64, Vec<Patch>),
}
struct Worker {
    wake: mpsc::SyncSender<()>,
    pending: Arc<arc_swap::ArcSwapOption<Job>>,
    ready: Arc<arc_swap::ArcSwapOption<Ready>>,
    epoch: Arc<AtomicU64>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.pending.store(None);
    }
}
impl Worker {
    fn new(book_id: &str, ctx: &egui::Context) -> Self {
        let path = rebook_app_core::environment::paths().map(|p| {
            p.data_local_dir().join("pdf-views").join(format!(
                "{}.json",
                rebook_app_core::persistence::safe_book_id(book_id)
            ))
        });
        let (wake, rx) = mpsc::sync_channel(1);
        let pending = Arc::new(arc_swap::ArcSwapOption::<Job>::empty());
        let jobs = pending.clone();
        let ready = Arc::new(arc_swap::ArcSwapOption::<Ready>::empty());
        let result = ready.clone();
        let epoch = Arc::new(AtomicU64::new(0));
        let generation = epoch.clone();
        let initial_ctx = ctx.clone();
        std::thread::Builder::new()
            .name("pdf-viewport".into())
            .spawn(move || run_worker(rx, jobs, result, generation, path, initial_ctx))
            .expect("PDF viewport worker");
        Self {
            wake,
            pending,
            ready,
            epoch,
        }
    }
}
fn run_worker(
    wake: mpsc::Receiver<()>,
    jobs: Arc<arc_swap::ArcSwapOption<Job>>,
    result: Arc<arc_swap::ArcSwapOption<Ready>>,
    epoch: Arc<AtomicU64>,
    path: Option<PathBuf>,
    ctx: egui::Context,
) {
    let mode = path
        .as_ref()
        .and_then(|p| {
            std::fs::metadata(p)
                .ok()
                .filter(|m| m.len() < 1024)
                .and_then(|_| std::fs::read(p).ok())
        })
        .and_then(|bytes| serde_json::from_slice::<Mode>(&bytes).ok())
        .filter(
            |mode| !matches!(mode,Mode::Manual(v) if !v.is_finite() || !(25.0..=400.0).contains(v)),
        )
        .unwrap_or_default();
    result.store(Some(Arc::new(Ready::Loaded(mode))));
    ctx.request_repaint();
    while wake.recv().is_ok() {
        let Some(job) = jobs.swap(None) else { continue };
        if epoch.load(Ordering::Acquire) != job.generation {
            continue;
        }
        if let Some(path) = &path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(error) = rebook_app_core::persistence::write_json_atomic(path, &job.mode) {
                tracing::warn!(%error,"Unable to save PDF zoom preference");
            }
        }
        let mut patches = Vec::new();
        for region in &job.regions {
            if epoch.load(Ordering::Acquire) != job.generation {
                break;
            }
            match job
                .source
                .fixed_page_region(region.page, region.normalized, region.pixels)
            {
                Ok(Some(raster)) => patches.push(Patch {
                    page: region.page,
                    background: peniko::Color::from_rgba8(
                        job.background.red,
                        job.background.green,
                        job.background.blue,
                        job.background.alpha,
                    ),
                    rect: kurbo::Rect::new(
                        region.rect[0] as f64,
                        region.rect[1] as f64,
                        (region.rect[0] + region.rect[2]) as f64,
                        (region.rect[1] + region.rect[3]) as f64,
                    ),
                    image: peniko::ImageData {
                        data: Blob::new(Arc::new(raster.pixels)),
                        format: peniko::ImageFormat::Rgba8,
                        alpha_type: peniko::ImageAlphaType::Alpha,
                        width: raster.width,
                        height: raster.height,
                    },
                }),
                Err(error) => {
                    tracing::warn!(%error,page=region.page,"Unable to refine PDF viewport")
                }
                Ok(None) => {}
            }
        }
        if epoch.load(Ordering::Acquire) == job.generation {
            result.store(Some(Arc::new(Ready::Rendered(job.generation, patches))));
            job.ctx.request_repaint();
        }
    }
}
#[derive(Default)]
pub(super) struct State {
    pub transform: FixedPageTransform,
    mode: Mode,
    worker: Option<Worker>,
    patches: Vec<Patch>,
    bounds: Option<Rect>,
    geometry: Vec<(usize, [f32; 4])>,
    canvas: Vec2,
    actual_scale: f32,
    background: Option<Rgba>,
    requested: Option<(Vec<Region>, Mode)>,
    deadline: Option<Instant>,
    touched: bool,
    initialized: bool,
    pub(super) scroll_target: Option<f32>,
    pub(super) preserve_next_scroll: bool,
    pub(super) progress_lock: Option<f32>,
}
impl State {
    pub(super) fn affine(&self) -> Affine {
        Affine::translate((
            self.transform.offset[0] as f64,
            self.transform.offset[1] as f64,
        )) * Affine::scale(self.transform.scale as f64)
    }
    pub(super) fn patch(
        &self,
        page: usize,
    ) -> Option<(&peniko::ImageData, kurbo::Rect, peniko::Color)> {
        if self.deadline.is_some() {
            return None;
        }
        self.patches
            .iter()
            .find(|p| p.page == page)
            .map(|p| (&p.image, p.rect, p.background))
    }
    fn changed(&mut self, ctx: &egui::Context) {
        self.requested = None;
        self.deadline = Some(Instant::now() + Duration::from_millis(140));
        if let Some(w) = &self.worker {
            w.epoch.fetch_add(1, Ordering::AcqRel);
            w.pending.store(None);
        }
        ctx.request_repaint_after(Duration::from_millis(140));
    }
    fn percentage(&self) -> f32 {
        self.transform.scale / self.actual_scale.max(0.001) * 100.
    }
    fn set_mode(&mut self, mode: Mode, anchor: [f32; 2], ctx: &egui::Context) {
        let Some(bounds) = self.bounds else {
            self.mode = mode;
            self.touched = true;
            return;
        };
        self.mode = mode;
        self.touched = true;
        let scale = match mode {
            Mode::FitPage => 1.,
            Mode::FitWidth => self.canvas.x / bounds.width(),
            Mode::Actual => self.actual_scale,
            Mode::Manual(p) => self.actual_scale * p.clamp(25., 400.) / 100.,
        };
        self.transform.zoom_at(scale, anchor);
        self.transform.constrain(
            [bounds.left(), bounds.top(), bounds.width(), bounds.height()],
            self.canvas.into(),
        );
        self.changed(ctx);
    }
}
impl DesktopReader {
    fn set_pdf_mode(&mut self, mode: Mode, anchor: [f32; 2], ctx: &egui::Context) {
        let previous = self.pdf_zoom.transform.scale;
        self.pdf_zoom.set_mode(mode, anchor, ctx);
        if self.is_scroll_mode() {
            let scale = self.pdf_zoom.transform.scale;
            if (previous - scale).abs() > 0.001 {
                let offset = self.scroll_viewport.map_or(0., |v| v.offset_y);
                let target = (offset + anchor[1] / previous - anchor[1] / scale).max(0.);
                self.pdf_zoom.scroll_target = Some(target);
                self.pdf_zoom.preserve_next_scroll = true;
                // Retain the pointer anchor in this first frame. The next
                // ScrollArea pass applies the source offset and clears this.
                self.pdf_zoom.transform.offset[1] = -(target - offset) * scale;
            } else {
                self.pdf_zoom.transform.offset[1] = 0.;
            }
        }
    }

    pub(super) fn pdf_zoom_enabled(&self) -> bool {
        self.format == BookFormat::Pdf && self.pdf_ocr.mode == PdfOcrViewMode::Original
    }
    pub(super) fn pdf_view_affine(&self) -> Affine {
        if self.pdf_zoom_enabled() {
            self.pdf_zoom.affine()
        } else {
            Affine::IDENTITY
        }
    }
    fn pdf_geometry(&mut self) -> Vec<(usize, Rect, Rect)> {
        if self.is_scroll_mode() {
            let Some(v) = self.scroll_viewport else {
                return vec![];
            };
            let Ok(layout) = self.current_scroll_layout() else {
                return vec![];
            };
            layout
                .pages
                .iter()
                .enumerate()
                .filter_map(|(i, p)| {
                    let local = p.page.image_bounds()?;
                    let offset = layout.page_tops[i] - v.offset_y - layout.page_origins[i];
                    let scene = local + kurbo::Vec2::new(0., offset as f64);
                    (scene.y1 > 0. && scene.y0 < v.size.y as f64).then_some((
                        p.position.section_index,
                        to_egui(scene),
                        to_egui(local),
                    ))
                })
                .take(2)
                .collect()
        } else {
            let positions = self.reader.current_spread_positions().unwrap_or_default();
            let Ok(spread) = self.reader.current_spread() else {
                return vec![];
            };
            let mut out = Vec::new();
            for (i, (page, offset)) in std::iter::once((&spread.primary, spread.primary_offset_x))
                .chain(
                    spread
                        .secondary
                        .as_ref()
                        .map(|p| (p, spread.secondary_offset_x)),
                )
                .enumerate()
            {
                if let Some(bounds) = page.image_bounds() {
                    out.push((
                        positions
                            .get(i)
                            .map_or(self.snapshot.location.section_index, |p| p.section_index),
                        to_egui(bounds + kurbo::Vec2::new(offset as f64, 0.)),
                        to_egui(bounds),
                    ));
                }
            }
            out
        }
    }
    pub(super) fn pdf_canvas_input(&mut self, response: &egui::Response, blocked: bool) -> bool {
        if !self.pdf_zoom_enabled() {
            if self.pdf_zoom.requested.is_some() || !self.pdf_zoom.patches.is_empty() {
                if let Some(w) = &self.pdf_zoom.worker {
                    w.epoch.fetch_add(1, Ordering::AcqRel);
                    w.pending.store(None);
                    w.ready.store(None);
                }
                self.pdf_zoom.patches.clear();
                self.pdf_zoom.requested = None;
                self.pdf_zoom.geometry.clear();
                self.pdf_zoom.bounds = None;
            }
            return false;
        }
        let ctx = &response.ctx;
        if self.pdf_zoom.worker.is_none() {
            self.pdf_zoom.worker = Some(Worker::new(&self.book_id, ctx));
        }
        let mut dirty = false;
        if let Some(ready) = self
            .pdf_zoom
            .worker
            .as_ref()
            .and_then(|w| w.ready.swap(None))
        {
            if let Ok(ready) = Arc::try_unwrap(ready) {
                match ready {
                    Ready::Loaded(mode) => {
                        self.pdf_zoom.initialized = true;
                        if !self.pdf_zoom.touched {
                            self.pdf_zoom.mode = mode;
                            self.pdf_zoom.bounds = None;
                            dirty = true;
                        }
                    }
                    Ready::Rendered(g, patches)
                        if self
                            .pdf_zoom
                            .worker
                            .as_ref()
                            .is_some_and(|w| g == w.epoch.load(Ordering::Acquire)) =>
                    {
                        self.pdf_zoom.patches = patches;
                        dirty = true;
                    }
                    _ => {}
                }
            }
        }
        let geometry = self.pdf_geometry();
        let Some(bounds) = geometry
            .iter()
            .map(|(_, b, _)| *b)
            .reduce(|a, b| a.union(b))
        else {
            return false;
        };
        let signature = geometry
            .iter()
            .map(|(p, _, local)| {
                (
                    *p,
                    [local.left(), local.top(), local.width(), local.height()],
                )
            })
            .collect::<Vec<_>>();
        let canvas = response.rect.size();
        let points = geometry
            .first()
            .and_then(|(page, _, _)| self.source.fixed_page_points(*page));
        let actual = points.map_or(1., |p| p[0] * 96. / 72. / geometry[0].1.width());
        let background = self.reader.style().background;
        if self.pdf_zoom.bounds != Some(bounds)
            || self.pdf_zoom.canvas != canvas
            || self.pdf_zoom.geometry != signature
            || self.pdf_zoom.background != Some(background)
            || self.pdf_zoom.actual_scale != actual
        {
            let page_changed = self.pdf_zoom.geometry != signature;
            self.pdf_zoom.background = Some(background);
            self.pdf_zoom.geometry = signature;
            self.pdf_zoom.bounds = Some(bounds);
            self.pdf_zoom.canvas = canvas;
            self.pdf_zoom.actual_scale = actual;
            if page_changed {
                self.pdf_zoom.patches.clear();
                if !self.is_scroll_mode() {
                    self.pdf_zoom.transform.offset = [0., 0.];
                }
            }
            let mode = self.pdf_zoom.mode;
            let touched = self.pdf_zoom.touched;
            self.set_pdf_mode(mode, (canvas * 0.5).into(), ctx);
            self.pdf_zoom.touched = touched;
            dirty = true;
        }
        let mut captured = false;
        if !blocked
            && !self.preview_is_open()
            && !self.ui.overlay_visible()
            && !ctx.text_edit_focused()
        {
            let hovered = response.hovered();
            let zoom = if hovered {
                ctx.input(|i| i.zoom_delta())
            } else {
                1.
            };
            let keyboard = ctx.input_mut(|i| {
                if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Equals)
                    || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Plus)
                {
                    1.25
                } else if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Minus) {
                    0.8
                } else if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num0) {
                    0.
                } else {
                    1.
                }
            });
            if zoom != 1. || keyboard != 1. {
                let anchor = response
                    .hover_pos()
                    .map_or(canvas * 0.5, |p| p - response.rect.min);
                let mode = if keyboard == 0. {
                    Mode::FitPage
                } else {
                    Mode::Manual((self.pdf_zoom.percentage() * zoom * keyboard).clamp(25., 400.))
                };
                self.set_pdf_mode(mode, anchor.into(), ctx);
                dirty = true;
                captured = true;
            }
            let over_text = response.double_clicked()
                && response.interact_pointer_pos().is_some_and(|p| {
                    let p = self.pdf_document_point(p - response.rect.min);
                    self.hit_test_canvas(p.x, p.y, true)
                        .is_ok_and(|hit| hit.is_some())
                });
            if response.double_clicked() && !over_text {
                let mode = if self.pdf_zoom.mode == Mode::FitWidth {
                    Mode::FitPage
                } else {
                    Mode::FitWidth
                };
                self.set_pdf_mode(mode, (canvas * 0.5).into(), ctx);
                dirty = true;
                captured = true;
            }
            let pan_drag = response.dragged()
                && ctx.input(|i| i.key_down(egui::Key::Space) || i.pointer.middle_down());
            let delta = if captured {
                Vec2::ZERO
            } else if pan_drag {
                response.drag_delta()
            } else if hovered && self.pdf_zoom.transform.scale > 1.01 && !self.is_scroll_mode() {
                ctx.input(|i| i.smooth_scroll_delta)
            } else {
                Vec2::ZERO
            };
            if delta != Vec2::ZERO {
                self.pdf_zoom.transform.offset[0] += delta.x;
                self.pdf_zoom.transform.offset[1] += delta.y;
                self.pdf_zoom.transform.constrain(
                    [bounds.left(), bounds.top(), bounds.width(), bounds.height()],
                    canvas.into(),
                );
                if self.is_scroll_mode() {
                    let max = self.current_scroll_layout().ok().map_or(0., |layout| {
                        (layout.content_height - self.scroll_viewport.map_or(0., |v| v.size.y))
                            .max(0.)
                    });
                    if let Some(viewport) = &mut self.scroll_viewport {
                        let target = (viewport.offset_y - delta.y / self.pdf_zoom.transform.scale)
                            .clamp(0., max);
                        viewport.offset_y = target;
                        self.pdf_zoom.scroll_target = Some(target);
                        self.pdf_zoom.preserve_next_scroll = false;
                        self.pdf_zoom.progress_lock = None;
                    }
                    self.pdf_zoom.transform.offset[1] = 0.;
                }
                self.pdf_zoom.changed(ctx);
                dirty = true;
                captured = true;
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            }
        }
        if dirty {
            self.bump_scene_revision();
            ctx.request_repaint();
        }
        let mut regions = Vec::new();
        let transform = self.pdf_zoom.transform;
        let raster_scale =
            bounded_pixels(canvas * ctx.pixels_per_point())[0] as f32 / canvas.x.max(1.);
        if transform.scale > 1.01 {
            for (page, scene, local) in geometry {
                let min = transform.to_document([0., 0.]);
                let max = transform.to_document(canvas.into());
                let visible = Rect::from_min_max(Pos2::from(min), Pos2::from(max)).intersect(scene);
                if !visible.is_positive() {
                    continue;
                }
                let normalized = [
                    (visible.left() - scene.left()) / scene.width(),
                    (visible.top() - scene.top()) / scene.height(),
                    visible.width() / scene.width(),
                    visible.height() / scene.height(),
                ];
                let rect = [
                    local.left() + normalized[0] * local.width(),
                    local.top() + normalized[1] * local.height(),
                    normalized[2] * local.width(),
                    normalized[3] * local.height(),
                ];
                let pixels = bounded_pixels(visible.size() * transform.scale * raster_scale);
                regions.push(Region {
                    page,
                    rect,
                    normalized,
                    pixels,
                });
            }
        }
        let key = (regions.clone(), self.pdf_zoom.mode);
        // Do not queue/save the initial FitPage default before the worker has
        // read a persisted preference. Explicit user input may supersede it.
        if (self.pdf_zoom.initialized || self.pdf_zoom.touched)
            && self.pdf_zoom.requested.as_ref() != Some(&key)
        {
            let at = self.pdf_zoom.deadline.get_or_insert_with(Instant::now);
            if Instant::now() >= *at {
                self.pdf_zoom.deadline = None;
                self.pdf_zoom.requested = Some(key);
                let w = self.pdf_zoom.worker.as_ref().unwrap();
                let generation = w.epoch.fetch_add(1, Ordering::AcqRel) + 1;
                w.pending.store(Some(Arc::new(Job {
                    generation,
                    source: self.source.clone(),
                    regions,
                    mode: self.pdf_zoom.mode,
                    background: self.reader.style().background,
                    ctx: ctx.clone(),
                })));
                let _ = w.wake.try_send(());
            } else {
                ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
            }
        }
        captured || ctx.input(|i| i.key_down(egui::Key::Space) && i.pointer.primary_down())
    }
    pub(super) fn pdf_zoom_controls(
        &mut self,
        ctx: &egui::Context,
        page_rect: Rect,
        blocked: bool,
    ) {
        if !self.pdf_zoom_enabled()
            || blocked
            || self.preview_is_open()
            || self.ui.overlay_visible()
            || page_rect.width() < 152.
            || page_rect.height() < 68.
        {
            return;
        }
        let anchor = (self.pdf_zoom.canvas * 0.5).into();
        let percentage = self.pdf_zoom.percentage();
        let mut factor = None;
        egui::Area::new(egui::Id::unique("pdf-zoom-controls"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(page_rect.right_bottom() - egui::vec2(20., 20.))
            .movable(false)
            .show(ctx, |ui| {
                ui.set_clip_rect(page_rect);
                let colors = crate::ui::palette();
                egui::Frame::new()
                    .fill(colors.surface)
                    .stroke(egui::Stroke::new(1., colors.border))
                    .corner_radius(12)
                    .inner_margin(4)
                    .shadow(egui::Shadow {
                        offset: [0, 2],
                        blur: 10,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(20),
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 2.;
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled_ui(percentage > 25.01, |ui| {
                                    crate::ui::icon_button(ui, crate::ui::Icon::Minus)
                                })
                                .inner
                                .on_hover_text(self.language.text("缩小", "Zoom out"))
                                .clicked()
                            {
                                factor = Some(0.8);
                            }
                            ui.add_sized(
                                [52., 32.],
                                egui::Label::new(
                                    egui::RichText::new(format!("{percentage:.0}%"))
                                        .size(crate::ui::scaled_font_size(13.))
                                        .color(colors.text),
                                )
                                .sense(egui::Sense::hover()),
                            );
                            if ui
                                .add_enabled_ui(percentage < 399.99, |ui| {
                                    crate::ui::icon_button(ui, crate::ui::Icon::Plus)
                                })
                                .inner
                                .on_hover_text(self.language.text("放大", "Zoom in"))
                                .clicked()
                            {
                                factor = Some(1.25);
                            }
                        });
                    });
            });
        if let Some(factor) = factor {
            self.set_pdf_mode(
                Mode::Manual((percentage * factor).clamp(25., 400.)),
                anchor,
                ctx,
            );
            self.bump_scene_revision();
            ctx.request_repaint();
        }
    }
    pub(super) fn pdf_document_point(&self, p: Vec2) -> Vec2 {
        if self.pdf_zoom_enabled() {
            Vec2::from(self.pdf_zoom.transform.to_document(p.into()))
        } else {
            p
        }
    }
}
fn to_egui(r: kurbo::Rect) -> Rect {
    Rect::from_min_max(
        egui::pos2(r.x0 as f32, r.y0 as f32),
        egui::pos2(r.x1 as f32, r.y1 as f32),
    )
}
fn bounded_pixels(size: Vec2) -> [u32; 2] {
    let factor = (4096. / size.x.max(size.y).max(1.))
        .min((8. * 1024. * 1024. / (size.x * size.y).max(1.)).sqrt())
        .min(1.);
    [
        (size.x * factor).floor().max(1.) as u32,
        (size.y * factor).floor().max(1.) as u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modes_constrain_pan_and_raster_budget_without_changing_source_geometry() {
        let ctx = egui::Context::default();
        let mut state = State {
            bounds: Some(Rect::from_min_size(
                egui::pos2(150., 0.),
                egui::vec2(500., 700.),
            )),
            canvas: egui::vec2(800., 700.),
            actual_scale: 1.5,
            ..Default::default()
        };
        state.set_mode(Mode::Manual(200.), [400., 350.], &ctx);
        assert_eq!(state.percentage(), 200.);
        assert_eq!(state.transform.scale, 3.);
        let source = [300., 210.];
        let view = state.transform.to_view(source);
        assert_eq!(state.transform.to_document(view), source);
        state.set_mode(Mode::FitWidth, [400., 350.], &ctx);
        assert_eq!(state.transform.scale, 1.6);
        state.set_mode(Mode::FitPage, [400., 350.], &ctx);
        assert_eq!(state.transform, FixedPageTransform::default());
        let size = bounded_pixels(egui::vec2(14000., 9000.));
        assert!(size[0] <= 4096 && size[1] <= 4096);
        assert!(u64::from(size[0]) * u64::from(size[1]) <= 8 * 1024 * 1024);
    }
}

#[cfg(test)]
mod local_pdf_tests {
    use super::*;
    fn local_source() -> Arc<dyn BookSource> {
        let path = std::env::var_os("TORTO_PDF_ZOOM_BOOK").expect("TORTO_PDF_ZOOM_BOOK");
        let raw = rebook_formats::open_file_for_reading(&path, None)
            .unwrap()
            .source();
        let original: Arc<dyn BookSource> =
            crate::plugins::cache_pdf_original_source(Path::new(&path), raw);
        let pipeline = rebook_app_core::reading::ReadingPipeline::new(
            original.clone(),
            original.book().id.as_str(),
            &PluginSettings::default(),
            crate::fonts::reader_fonts(),
        );
        Arc::new(super::super::media_links::Source::new(
            pipeline.structure,
            original,
        ))
    }

    #[test]
    #[ignore = "requires TORTO_PDF_ZOOM_BOOK and a native GPU; reads local PDF without AI or credentials"]
    fn local_pdf_zoom_keeps_source_hits_and_renders_only_the_visible_region() {
        let source = local_source();
        let page_index = 30.min(source.book().sections.len() - 1);
        let section = source.parse_section(page_index).unwrap();
        let anchor = section
            .blocks
            .iter()
            .find_map(|b| super::super::block_source_range(b))
            .unwrap()
            .start
            .clone();
        let (mut desktop, _, _) = super::super::semantic_layout::tests::fixture();
        desktop.source = source.clone();
        desktop.reader = ReaderSession::open_with_fonts(
            source.clone(),
            LayoutViewport::new(800, 700).unwrap(),
            ReaderStyle {
                spread: SpreadMode::Single,
                ..Default::default()
            },
            crate::fonts::reader_fonts(),
        )
        .unwrap();
        desktop.reader.go_to_source(&anchor).unwrap();
        desktop.snapshot = desktop.reader.snapshot();
        desktop.format = BookFormat::Pdf;
        desktop.pdf_ocr.mode = PdfOcrViewMode::Original;
        desktop.reading_mode = ReadingMode::Classic;
        let (wake, _rx) = mpsc::sync_channel(1);
        desktop.pdf_zoom.worker = Some(Worker {
            wake,
            pending: Default::default(),
            ready: Default::default(),
            epoch: Arc::new(AtomicU64::new(0)),
        });
        let ctx = egui::Context::default();
        let frame = |desktop: &mut DesktopReader, events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 700.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let (_, response) = ui
                        .allocate_exact_size(Vec2::new(800., 700.), egui::Sense::click_and_drag());
                    desktop.pdf_canvas_input(&response, false);
                },
            );
            output.textures_delta.clear();
        };
        frame(&mut desktop, vec![]);
        assert!(
            desktop
                .pdf_zoom
                .worker
                .as_ref()
                .unwrap()
                .pending
                .load()
                .is_none(),
            "do not save defaults before loading the preference"
        );
        desktop
            .pdf_zoom
            .worker
            .as_ref()
            .unwrap()
            .ready
            .store(Some(Arc::new(Ready::Loaded(Mode::Manual(100.)))));
        frame(&mut desktop, vec![]);
        assert_eq!(desktop.pdf_zoom.percentage(), 100.);
        desktop.pdf_zoom.set_mode(Mode::FitPage, [400., 350.], &ctx);
        frame(&mut desktop, vec![]);
        let locator = desktop.reader.current_locator();
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };
        let previous = desktop.pdf_zoom.transform.scale;
        frame(&mut desktop, vec![key(egui::Key::Equals)]);
        assert!(desktop.pdf_zoom.transform.scale > previous);
        assert_eq!(
            ctx.zoom_factor(),
            1.0,
            "PDF zoom must not resize the application UI"
        );
        frame(
            &mut desktop,
            vec![egui::Event::PointerMoved(egui::pos2(400., 350.))],
        );
        let previous = desktop.pdf_zoom.transform.scale;
        frame(&mut desktop, vec![egui::Event::Zoom(1.25)]);
        assert!(
            desktop.pdf_zoom.transform.scale > previous,
            "pinch gesture zooms the page"
        );
        frame(&mut desktop, vec![key(egui::Key::Num0)]);
        assert_eq!(desktop.pdf_zoom.transform.scale, 1.0);

        let mut audit = crate::platform::LayerAudit::new([800, 700]);
        let mut plan = super::super::ReaderFramePlan {
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 700.)),
            scene_id: desktop.scene_id,
            scene_revision: desktop.scene_revision,
            background: peniko::Color::from_rgb8(248, 247, 243),
            content_left: 400.,
            preview_left: 400.,
            reflow_pending: false,
            live_resize_preview: false,
        };
        let dir = std::env::var_os("TORTO_FEATURE_ARTIFACTS").map(PathBuf::from);
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir).unwrap();
            audit.direct(&desktop.page_scene(), plan, 1.);
            image::save_buffer(
                dir.join("fit.png"),
                &audit.pixels(),
                800,
                700,
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
        desktop
            .pdf_zoom
            .set_mode(Mode::Manual(200.), [400., 350.], &ctx);
        desktop.pdf_zoom.deadline = Some(Instant::now());
        frame(&mut desktop, vec![]);
        let request = desktop
            .pdf_zoom
            .worker
            .as_ref()
            .unwrap()
            .pending
            .swap(None)
            .unwrap();
        assert!(!request.regions.is_empty());
        assert!(request.regions.len() <= 2);
        for region in &request.regions {
            assert!(u64::from(region.pixels[0]) * u64::from(region.pixels[1]) <= 8 * 1024 * 1024);
            let raster = source
                .fixed_page_region(region.page, region.normalized, region.pixels)
                .unwrap()
                .unwrap();
            desktop.pdf_zoom.patches.push(Patch {
                page: region.page,
                rect: kurbo::Rect::new(
                    region.rect[0] as f64,
                    region.rect[1] as f64,
                    (region.rect[0] + region.rect[2]) as f64,
                    (region.rect[1] + region.rect[3]) as f64,
                ),
                background: plan.background,
                image: peniko::ImageData {
                    data: Blob::new(Arc::new(raster.pixels)),
                    width: raster.width,
                    height: raster.height,
                    format: peniko::ImageFormat::Rgba8,
                    alpha_type: peniko::ImageAlphaType::Alpha,
                },
            });
        }
        let mut found = None;
        'outer: for y in (40..660).step_by(12) {
            for x in (40..760).step_by(12) {
                let p = desktop.pdf_document_point(Vec2::new(x as f32, y as f32));
                if let Some(hit) = desktop.hit_test_canvas(p.x, p.y, true).unwrap() {
                    found = Some(hit);
                    break 'outer;
                }
            }
        }
        let has_text=section.blocks.iter().any(|b|matches!(b,Block::Image(i) if i.text_layer.as_ref().is_some_and(|t|!t.spans.is_empty())));
        if has_text {
            let hit = found.expect("a visible zoomed text glyph remains selectable");
            desktop.selection = desktop
                .reader
                .selection_between_with_granularity(&hit, &hit, SelectionGranularity::Word)
                .unwrap();
            assert!(desktop.selection.is_some());
        }
        desktop.bump_scene_revision();
        plan.scene_revision = desktop.scene_revision;
        let scene = desktop.page_scene();
        assert!(
            scene.images.len() >= 2,
            "visible region is painted over the base image"
        );
        audit.direct(&scene, plan, 1.);
        if let Some(dir) = &dir {
            image::save_buffer(
                dir.join("zoom-200.png"),
                &audit.pixels(),
                800,
                700,
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
        assert_eq!(
            desktop.reader.current_locator(),
            locator,
            "zoom and selection do not navigate"
        );
        assert_eq!(source.parse_section(page_index).unwrap(), section);
        println!(
            "PDF_ZOOM book={:?} page={} regions={} text={} scale={}",
            source.book().metadata.title,
            page_index + 1,
            request.regions.len(),
            has_text,
            desktop.pdf_zoom.transform.scale
        );
    }
    #[test]
    #[ignore = "requires TORTO_PDF_ZOOM_BOOK; validates real PDF continuous scrolling"]
    fn local_pdf_scroll_zoom_keeps_wheel_distance_and_reaches_page_bottom() {
        let source = local_source();
        let page = 30.min(source.book().sections.len() - 1);
        let section = source.parse_section(page).unwrap();
        let anchor = section
            .blocks
            .iter()
            .find_map(|b| super::super::block_source_range(b))
            .unwrap()
            .start
            .clone();
        let (mut desktop, _, _) = super::super::semantic_layout::tests::fixture();
        desktop.source = source.clone();
        desktop.reader = ReaderSession::open_with_fonts(
            source,
            LayoutViewport::new(800, 700).unwrap(),
            ReaderStyle {
                spread: SpreadMode::Scroll,
                ..Default::default()
            },
            crate::fonts::reader_fonts(),
        )
        .unwrap();
        desktop.reader.go_to_source(&anchor).unwrap();
        desktop.snapshot = desktop.reader.snapshot();
        desktop.format = BookFormat::Pdf;
        desktop.pdf_ocr.mode = PdfOcrViewMode::Original;
        desktop.reading_mode = ReadingMode::Classic;
        desktop.scroll_section = None;
        desktop.scroll_viewport = None;
        desktop.scroll_target_position = Some(snapshot_position(&desktop.snapshot));
        let (wake, _rx) = mpsc::sync_channel(1);
        desktop.pdf_zoom.worker = Some(Worker {
            wake,
            pending: Default::default(),
            ready: Default::default(),
            epoch: Arc::new(AtomicU64::new(0)),
        });
        let ctx = egui::Context::default();
        let frame = |desktop: &mut DesktopReader, events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 700.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    desktop.scroll_content(
                        ui,
                        Vec2::new(800., 700.),
                        None,
                        egui::Color32::WHITE,
                        false,
                    );
                },
            );
            output.textures_delta.clear();
        };
        for _ in 0..20 {
            frame(&mut desktop, vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
        let before = desktop.reader.current_locator();
        desktop.set_pdf_mode(Mode::Manual(200.), [400., 350.], &ctx);
        for _ in 0..3 {
            frame(&mut desktop, vec![]);
        }
        let scale = desktop.pdf_zoom.transform.scale;
        assert!(scale > 1.);
        assert!((desktop.scroll_viewport.unwrap().size.y * scale - 700.).abs() < 0.1);
        assert_eq!(
            desktop.reader.current_locator(),
            before,
            "zoom alone does not navigate"
        );
        frame(
            &mut desktop,
            vec![egui::Event::PointerMoved(egui::pos2(400., 350.))],
        );
        let offset = desktop.scroll_viewport.unwrap().offset_y;
        frame(
            &mut desktop,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                source: egui::MouseWheelSource::Unknown,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0., -60.),
                modifiers: egui::Modifiers::NONE,
            }],
        );
        for _ in 0..12 {
            frame(&mut desktop, vec![]);
        }
        let moved = (desktop.scroll_viewport.unwrap().offset_y - offset) * scale;
        assert!(
            moved > 0. && moved <= 65.,
            "physical wheel travel must not be multiplied by zoom: {moved}"
        );
        let layout = desktop.current_scroll_layout().unwrap();
        let bottom = (layout.content_height - desktop.scroll_viewport.unwrap().size.y).max(0.);
        desktop.pdf_zoom.scroll_target = Some(bottom);
        desktop.pdf_zoom.preserve_next_scroll = false;
        frame(&mut desktop, vec![]);
        let viewport = desktop.scroll_viewport.unwrap();
        assert!(
            (viewport.offset_y + viewport.size.y - layout.content_height).abs() < 1.,
            "zoomed page bottom remains reachable"
        );
        println!("PDF_SCROLL_ZOOM scale={scale} physical_wheel={moved} bottom={bottom}");
    }
}
