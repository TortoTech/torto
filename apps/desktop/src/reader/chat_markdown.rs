use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use egui::{Color32, FontFamily, FontId, ImageSource, RichText, Sense, Stroke, TextStyle, Vec2};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::plugins::CHAT_CITATION_PREFIX;
use crate::preferences::AppLanguage;
use crate::ui::{Icon, icon_button, palette};

use super::render::text_selection_fill;

const MARKDOWN_FONT_SIZE: f32 = 13.0;
const MARKDOWN_HEADING_FONT_SIZE: f32 = 16.0;
const PREVIEW_GAP: f32 = 8.0;
const ASSET_REPAINT_INTERVAL: Duration = Duration::from_millis(50);
const ASSET_RENDER_TIMEOUT: Duration = Duration::from_secs(8);
const CITATION_ICON: &str = "↗";

fn markdown_font_size() -> f32 {
    crate::ui::scaled_font_size(MARKDOWN_FONT_SIZE)
}

fn markdown_heading_font_size() -> f32 {
    crate::ui::scaled_font_size(MARKDOWN_HEADING_FONT_SIZE)
}

pub(super) struct ChatMarkdownState {
    markdown_cache: CommonMarkCache,
    assets: HashMap<AssetKey, AssetStatus>,
    asset_sender: SyncSender<AssetRequest>,
    asset_receiver: Receiver<AssetResult>,
    preview_states: HashMap<u64, PreviewState>,
}

impl Default for ChatMarkdownState {
    fn default() -> Self {
        let (asset_sender, request_receiver) = mpsc::sync_channel(1);
        let (result_sender, asset_receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("rebook-chat-assets".into())
            .spawn(move || asset_worker(&request_receiver, &result_sender))
            .expect("chat asset worker should start");
        Self {
            markdown_cache: CommonMarkCache::default(),
            assets: HashMap::new(),
            asset_sender,
            asset_receiver,
            preview_states: HashMap::new(),
        }
    }
}

impl ChatMarkdownState {
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        source: &str,
        language: AppLanguage,
        message_ordinal: usize,
        streaming: bool,
    ) -> Option<String> {
        self.drain_asset_results();
        let blocks = split_renderable_blocks(source);
        let mut citation = None;
        let mut preview_ordinal = 0;
        for (index, block) in blocks.iter().enumerate() {
            match block {
                RenderBlock::Markdown(markdown) => {
                    citation = self.show_commonmark(ui, markdown).or(citation);
                }
                RenderBlock::Preview { kind, source } => {
                    self.show_code_preview(
                        ui,
                        *kind,
                        source,
                        language,
                        PreviewContext {
                            message_ordinal,
                            preview_ordinal,
                            streaming,
                        },
                    );
                    preview_ordinal += 1;
                }
            }
            if index + 1 < blocks.len() {
                ui.add_space(PREVIEW_GAP);
            }
        }
        citation
    }

    fn show_commonmark(&mut self, ui: &mut egui::Ui, markdown: &str) -> Option<String> {
        if markdown.trim().is_empty() {
            return None;
        }
        let strong = normalize_loose_strong_delimiters(markdown);
        let normalized = normalize_math_delimiters(strong.as_ref());
        let (iconized, citation_groups) = citation_icon_markdown_with_groups(normalized.as_ref());
        let markdown = iconized.as_ref();
        let layout_blocks = split_markdown_layout_blocks(markdown);
        let all_citation_locators = citation_locators(markdown);
        for locator in &all_citation_locators {
            self.markdown_cache.add_link_hook(locator);
        }
        let formula_assets = Arc::new(self.prepare_markdown_math(markdown));
        let cache = &mut self.markdown_cache;
        let mut clicked = None;
        for (index, block) in layout_blocks.iter().enumerate() {
            let block_clicked = match block {
                MarkdownLayoutBlock::Markdown(source) => {
                    show_commonmark_fragment(ui, cache, source, &formula_assets);
                    clicked_citation(cache, &all_citation_locators)
                }
                MarkdownLayoutBlock::Heading { level, source } => {
                    if citation_locators(source).is_empty() {
                        show_markdown_heading(ui, *level, source, index > 0);
                        None
                    } else {
                        if index > 0 {
                            ui.add_space(6.0);
                        }
                        let heading = format!("{} {source}", "#".repeat(*level));
                        show_commonmark_fragment(ui, cache, &heading, &formula_assets);
                        clicked_citation(cache, &all_citation_locators)
                    }
                }
                MarkdownLayoutBlock::Table(table) => {
                    show_markdown_table(ui, table, cache, &formula_assets, &all_citation_locators)
                }
            };
            clicked = clicked.or_else(|| {
                block_clicked.map(|locator| {
                    citation_groups
                        .get(&locator)
                        .map_or(locator, |group| group.join("\n"))
                })
            });
        }
        for locator in all_citation_locators {
            self.markdown_cache.remove_link_hook(&locator);
        }
        clicked
    }

    fn show_code_preview(
        &mut self,
        ui: &mut egui::Ui,
        kind: PreviewKind,
        source: &str,
        language: AppLanguage,
        context: PreviewContext,
    ) {
        // A streamed code block changes on every model delta. Keep the identity tied
        // to its position in the message so controls and the last good frame survive.
        let preview_id = preview_state_id(context.message_ordinal, context.preview_ordinal, kind);
        let mut state = self.preview_states.remove(&preview_id).unwrap_or_default();

        egui::Frame::new()
            .fill(palette().surface_muted)
            .stroke(egui::Stroke::new(1.0, palette().border))
            .corner_radius(7)
            .inner_margin(egui::Margin::symmetric(8, 7))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(kind.label())
                            .size(crate::ui::scaled_font_size(11.0))
                            .strong()
                            .color(palette().muted),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        preview_mode_control(ui, &mut state.mode, language);
                        if state.mode == PreviewMode::Preview {
                            ui.add_space(5.0);
                            let (size_icon, size_label) = if state.full_size {
                                (
                                    Icon::Minimize2,
                                    language.text("缩小预览", "Minimize preview"),
                                )
                            } else {
                                (Icon::Maximize2, language.text("放大预览", "Expand preview"))
                            };
                            if icon_button(ui, size_icon)
                                .on_hover_text(size_label)
                                .clicked()
                            {
                                state.full_size = !state.full_size;
                            }
                        }
                    });
                });

                ui.add_space(7.0);
                match state.mode {
                    PreviewMode::Preview => {
                        if state.full_size {
                            self.show_preview_result(
                                ui,
                                kind,
                                source,
                                language,
                                context.streaming,
                                &mut state,
                            );
                        } else {
                            egui::ScrollArea::vertical()
                                .max_height(260.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    self.show_preview_result(
                                        ui,
                                        kind,
                                        source,
                                        language,
                                        context.streaming,
                                        &mut state,
                                    );
                                });
                        }
                    }
                    PreviewMode::Code => show_source_code(ui, source),
                }
            });
        self.preview_states.insert(preview_id, state);
    }

    fn show_preview_result(
        &mut self,
        ui: &mut egui::Ui,
        kind: PreviewKind,
        source: &str,
        language: AppLanguage,
        streaming: bool,
        state: &mut PreviewState,
    ) {
        match kind {
            PreviewKind::Svg => {
                if is_svg_markup(source) {
                    paint_svg(ui, source.as_bytes(), "svg-preview", false);
                } else {
                    preview_error(ui, language.text("SVG 内容无效", "Invalid SVG content"));
                }
            }
            PreviewKind::Mermaid => {
                let key = AssetKey::Mermaid(source.to_owned());
                if let Some(pending) = state.pending_asset.as_ref()
                    && matches!(self.assets.get(pending), Some(AssetStatus::Ready(_)))
                {
                    state.displayed_asset = Some(pending.clone());
                }
                if self.queue_asset(key.clone()) {
                    state.pending_asset = Some(key.clone());
                }
                if matches!(self.assets.get(&key), Some(AssetStatus::Ready(_))) {
                    state.displayed_asset = Some(key.clone());
                }

                let displayed = match self.assets.get(&key) {
                    Some(AssetStatus::Ready(_)) => &key,
                    Some(AssetStatus::Error(_)) if !streaming => &key,
                    _ => state.displayed_asset.as_ref().unwrap_or(&key),
                };
                paint_preview_asset(
                    ui,
                    &self.assets,
                    displayed,
                    streaming,
                    &mut state.frame_height,
                    &mut state.frame_width,
                );
            }
        }
    }

    fn prepare_markdown_math(&mut self, markdown: &str) -> HashMap<AssetKey, AssetStatus> {
        let mut keys = Vec::new();
        for event in Parser::new_ext(markdown, markdown_options()) {
            let key = match event {
                Event::InlineMath(tex) => Some(AssetKey::formula(&tex, true)),
                Event::DisplayMath(tex) => Some(AssetKey::formula(&tex, false)),
                _ => None,
            };
            if let Some(key) = key {
                let _ = self.queue_asset(key.clone());
                keys.push(key);
            }
        }
        keys.into_iter()
            .filter_map(|key| self.assets.get(&key).cloned().map(|status| (key, status)))
            .collect()
    }

    fn queue_asset(&mut self, key: AssetKey) -> bool {
        if self.assets.contains_key(&key) {
            return true;
        }
        match self
            .asset_sender
            .try_send(AssetRequest { key: key.clone() })
        {
            Ok(()) => {
                self.assets.insert(
                    key,
                    AssetStatus::Pending {
                        started_at: Instant::now(),
                    },
                );
                true
            }
            Err(TrySendError::Full(_)) => {
                // Streaming Mermaid/SVG source changes on nearly every model delta.
                // Keep at most one waiting render and retry the newest source on the
                // next frame instead of accumulating obsolete partial diagrams.
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                crate::diagnostics::log(
                    "chat.asset.worker_unavailable",
                    &[crate::diagnostics::Field::Text("kind", asset_kind(&key))],
                );
                self.assets.insert(
                    key,
                    AssetStatus::Error("render worker is unavailable".to_owned()),
                );
                true
            }
        }
    }

    fn drain_asset_results(&mut self) {
        while let Ok(result) = self.asset_receiver.try_recv() {
            self.assets.insert(result.key, result.status);
        }
        for (key, status) in &mut self.assets {
            let AssetStatus::Pending { started_at } = status else {
                continue;
            };
            if started_at.elapsed() < ASSET_RENDER_TIMEOUT {
                continue;
            }
            crate::diagnostics::log(
                "chat.asset.timeout",
                &[crate::diagnostics::Field::Text("kind", asset_kind(key))],
            );
            *status = AssetStatus::Error("rendering timed out".to_owned());
        }
    }
}

fn show_commonmark_fragment(
    ui: &mut egui::Ui,
    cache: &mut CommonMarkCache,
    markdown: &str,
    formula_assets: &Arc<HashMap<AssetKey, AssetStatus>>,
) {
    if markdown.trim().is_empty() {
        return;
    }
    let formula_assets = Arc::clone(formula_assets);
    let math_renderer = move |ui: &mut egui::Ui, tex: &str, inline: bool| {
        paint_asset(ui, &formula_assets, &AssetKey::formula(tex, inline), inline);
    };
    let html_renderer = |ui: &mut egui::Ui, html: &str| {
        if is_svg_markup(html) {
            paint_svg(ui, html.as_bytes(), "inline-svg", false);
        } else {
            ui.add(
                egui::Label::new(RichText::new(html).monospace().size(markdown_font_size()))
                    .wrap()
                    .selectable(true),
            );
        }
    };
    ui.scope(|ui| {
        apply_markdown_text_visuals(ui);
        ui.style_mut().interaction.selectable_labels = true;
        ui.style_mut().text_styles.insert(
            TextStyle::Body,
            FontId::new(markdown_font_size(), FontFamily::Proportional),
        );
        ui.style_mut().text_styles.insert(
            TextStyle::Heading,
            FontId::new(markdown_heading_font_size(), FontFamily::Proportional),
        );
        ui.style_mut().text_styles.insert(
            TextStyle::Monospace,
            FontId::new(crate::ui::scaled_font_size(12.0), FontFamily::Monospace),
        );
        CommonMarkViewer::new()
            .indentation_spaces(2)
            .render_math_fn(Some(&math_renderer))
            .render_html_fn(Some(&html_renderer))
            .show(ui, cache, markdown);
    });
}

fn show_markdown_heading(ui: &mut egui::Ui, level: usize, source: &str, add_top_space: bool) {
    ui.scope(|ui| {
        apply_markdown_text_visuals(ui);
        if add_top_space {
            ui.add_space(6.0);
        }
        let size = match level {
            1 => crate::ui::scaled_font_size(17.0),
            2 => crate::ui::scaled_font_size(15.5),
            3 => crate::ui::scaled_font_size(14.5),
            4 => crate::ui::scaled_font_size(13.75),
            5 => crate::ui::scaled_font_size(13.25),
            _ => markdown_font_size(),
        };
        ui.add(
            egui::Label::new(
                RichText::new(markdown_plain_text(source))
                    .size(size)
                    .strong()
                    .family(markdown_strong_font_family())
                    .color(palette().text),
            )
            .wrap()
            .selectable(true),
        );
        ui.add_space(3.0);
    });
}

fn show_markdown_table(
    ui: &mut egui::Ui,
    table: &MarkdownTable,
    cache: &mut CommonMarkCache,
    formula_assets: &Arc<HashMap<AssetKey, AssetStatus>>,
    all_citation_locators: &[String],
) -> Option<String> {
    let column_count = table.rows.iter().map(Vec::len).max().unwrap_or_default();
    if column_count == 0 {
        return None;
    }
    let mut clicked = None;
    let table_width = ui.available_width().max(1.0);
    let display_column_count = u16::try_from(column_count).unwrap_or(u16::MAX);
    let cell_width = table_width / f32::from(display_column_count);
    ui.add_space(3.0);
    ui.scope(|ui| {
        apply_markdown_text_visuals(ui);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        for (row_index, row) in table.rows.iter().enumerate() {
            let row_height = markdown_table_row_height(ui, row, column_count, cell_width);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                for column_index in 0..column_count {
                    let width = if column_index + 1 == column_count {
                        (table_width
                            - cell_width * f32::from(display_column_count.saturating_sub(1)))
                        .max(1.0)
                    } else {
                        cell_width
                    };
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(width, row_height), Sense::hover());
                    let fill = if row_index == 0 {
                        palette().surface_muted
                    } else if row_index % 2 == 0 {
                        palette().card_fill
                    } else {
                        palette().surface
                    };
                    ui.painter().rect(
                        rect,
                        0,
                        fill,
                        egui::Stroke::new(1.0, palette().border),
                        egui::StrokeKind::Inside,
                    );

                    let content_rect = rect.shrink2(Vec2::new(6.0, 5.0));
                    let mut cell_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt((
                                "chat-markdown-table-cell",
                                stable_hash(&table.rows),
                                row_index,
                                column_index,
                            ))
                            .max_rect(content_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    cell_ui.set_clip_rect(content_rect.intersect(ui.clip_rect()));
                    let markdown = row.get(column_index).map_or("", String::as_str);
                    let text = markdown_plain_text(markdown);
                    if citation_locators(markdown).is_empty() {
                        let mut text = RichText::new(text)
                            .size(markdown_font_size())
                            .color(palette().text);
                        if row_index == 0 {
                            text = text.strong().family(markdown_strong_font_family());
                        }
                        cell_ui.add(egui::Label::new(text).wrap().selectable(true));
                    } else {
                        show_commonmark_fragment(&mut cell_ui, cache, markdown, formula_assets);
                        clicked = clicked
                            .take()
                            .or_else(|| clicked_citation(cache, all_citation_locators));
                    }
                }
            });
        }
    });
    ui.add_space(6.0);
    clicked
}

fn markdown_table_row_height(
    ui: &mut egui::Ui,
    row: &[String],
    column_count: usize,
    cell_width: f32,
) -> f32 {
    let content_width = (cell_width - 12.0).max(1.0);
    let font_id = FontId::new(markdown_font_size(), FontFamily::Proportional);
    let text_color = palette().text;
    let content_height = (0..column_count)
        .map(|column_index| {
            let text = row
                .get(column_index)
                .map_or_else(String::new, |cell| markdown_plain_text(cell));
            ui.fonts_mut(|fonts| {
                fonts
                    .layout(text, font_id.clone(), text_color, content_width)
                    .size()
                    .y
            })
        })
        .fold(0.0, f32::max);
    content_height.max(ui.text_style_height(&TextStyle::Body)) + 10.0
}

fn apply_markdown_text_visuals(ui: &mut egui::Ui) {
    let palette = palette();
    let surface = opaque_over(palette.surface, palette.background);
    let selection = opaque_over(text_selection_fill(), surface);
    let visuals = ui.visuals_mut();
    visuals.override_text_color = Some(palette.text);
    visuals.selection.bg_fill = selection;
    visuals.selection.stroke = Stroke::new(1.0, palette.text);
}

fn markdown_strong_font_family() -> FontFamily {
    FontFamily::Name(egui_commonmark_backend::STRONG_FONT_FAMILY.into())
}

fn opaque_over(foreground: Color32, background: Color32) -> Color32 {
    let [foreground_r, foreground_g, foreground_b, foreground_a] =
        foreground.to_srgba_unmultiplied();
    let [background_r, background_g, background_b, _] = background.to_srgba_unmultiplied();
    let alpha = u32::from(foreground_a);
    let blend = |foreground: u8, background: u8| {
        let value =
            u32::from(foreground) * alpha + u32::from(background) * (u32::from(u8::MAX) - alpha);
        u8::try_from((value + 127) / u32::from(u8::MAX)).unwrap_or(u8::MAX)
    };
    Color32::from_rgb(
        blend(foreground_r, background_r),
        blend(foreground_g, background_g),
        blend(foreground_b, background_b),
    )
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum AssetKey {
    Formula { tex: String, inline: bool },
    Mermaid(String),
}

impl AssetKey {
    fn formula(tex: &str, inline: bool) -> Self {
        Self::Formula {
            tex: tex.to_owned(),
            inline,
        }
    }
}

#[derive(Clone)]
enum AssetStatus {
    Pending { started_at: Instant },
    Ready(ReadyAsset),
    Error(String),
}

#[derive(Clone)]
struct ReadyAsset {
    svg: Arc<[u8]>,
    source_size: [f32; 2],
}

struct AssetRequest {
    key: AssetKey,
}

struct AssetResult {
    key: AssetKey,
    status: AssetStatus,
}

fn asset_worker(requests: &Receiver<AssetRequest>, results: &Sender<AssetResult>) {
    while let Ok(request) = requests.recv() {
        let status = match &request.key {
            AssetKey::Formula { tex, inline } => render_formula(tex, *inline)
                .and_then(ready_svg_asset)
                .unwrap_or_else(AssetStatus::Error),
            AssetKey::Mermaid(source) => mermaid_rs_renderer::render(source)
                .map_err(|error| error.to_string())
                .and_then(ready_svg_asset)
                .unwrap_or_else(AssetStatus::Error),
        };
        match &status {
            AssetStatus::Ready(asset) => crate::diagnostics::log(
                "chat.asset.ready",
                &[
                    crate::diagnostics::Field::Text("kind", asset_kind(&request.key)),
                    crate::diagnostics::Field::Usize("bytes", asset.svg.len()),
                    crate::diagnostics::Field::F32("width", asset.source_size[0]),
                    crate::diagnostics::Field::F32("height", asset.source_size[1]),
                ],
            ),
            AssetStatus::Error(error) => crate::diagnostics::log(
                "chat.asset.error",
                &[
                    crate::diagnostics::Field::Text("kind", asset_kind(&request.key)),
                    crate::diagnostics::Field::Usize("error_chars", error.chars().count()),
                ],
            ),
            AssetStatus::Pending { .. } => {}
        }
        if results
            .send(AssetResult {
                key: request.key,
                status,
            })
            .is_err()
        {
            break;
        }
    }
}

const fn asset_kind(key: &AssetKey) -> &'static str {
    match key {
        AssetKey::Formula { .. } => "formula",
        AssetKey::Mermaid(_) => "mermaid",
    }
}

fn ready_svg_asset(svg: String) -> Result<AssetStatus, String> {
    let source_size = svg_source_size(svg.as_bytes())?;
    Ok(AssetStatus::Ready(ReadyAsset {
        svg: Arc::from(svg.into_bytes()),
        source_size,
    }))
}

fn svg_source_size(svg: &[u8]) -> Result<[f32; 2], String> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg, &options).map_err(|error| error.to_string())?;
    let size = tree.size();
    let source_size = [size.width(), size.height()];
    if source_size
        .iter()
        .any(|dimension| !dimension.is_finite() || *dimension <= 0.0)
    {
        return Err("SVG has invalid dimensions".to_owned());
    }
    Ok(source_size)
}

fn asset_display_size(
    source_size: [f32; 2],
    available_width: f32,
    inline: bool,
    fill_width: bool,
) -> Vec2 {
    let source = Vec2::new(source_size[0], source_size[1]);
    let scale = if inline {
        (22.0 / source.y).min(1.0)
    } else if fill_width {
        available_width.max(1.0) / source.x
    } else {
        (available_width.max(1.0) / source.x).min(1.0)
    };
    (source * scale).max(Vec2::splat(1.0))
}

fn render_formula(tex: &str, inline: bool) -> Result<String, String> {
    let font_size = if inline { 13.0 } else { 16.0 };
    let padding = if inline { 1.0 } else { 4.0 };
    let rendered = rebook_math::math::render_math(tex, font_size, "#262624", !inline)?;
    let width = (rendered.width + padding * 2.0).max(1.0);
    let height = (rendered.ascent + rendered.descent + padding * 2.0).max(1.0);
    let view_y = -rendered.ascent - padding;
    Ok(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}" width="{}" height="{}"><g transform="translate({}, 0)">{}</g></svg>"#,
        -padding, view_y, width, height, width, height, padding, rendered.svg_fragment
    ))
}

fn paint_asset(
    ui: &mut egui::Ui,
    assets: &HashMap<AssetKey, AssetStatus>,
    key: &AssetKey,
    inline: bool,
) {
    match assets.get(key) {
        Some(AssetStatus::Ready(asset)) => {
            let uri = format!("bytes://rebook-chat-{}.svg", stable_hash(key));
            let display_size = asset_display_size(
                asset.source_size,
                ui.available_width(),
                inline,
                matches!(key, AssetKey::Mermaid(_)),
            );
            let image = egui::Image::new(ImageSource::Bytes {
                uri: uri.into(),
                bytes: egui::load::Bytes::Shared(Arc::clone(&asset.svg)),
            })
            .fit_to_exact_size(display_size);
            ui.add(image);
        }
        Some(AssetStatus::Error(error)) => {
            preview_error(ui, error);
        }
        Some(AssetStatus::Pending { .. }) | None => {
            ui.ctx().request_repaint_after(ASSET_REPAINT_INTERVAL);
            if inline {
                ui.label(
                    RichText::new("…")
                        .size(markdown_font_size())
                        .color(palette().muted),
                );
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Rendering…")
                            .size(crate::ui::scaled_font_size(11.5))
                            .color(palette().muted),
                    );
                });
            }
        }
    }
}

fn paint_preview_asset(
    ui: &mut egui::Ui,
    assets: &HashMap<AssetKey, AssetStatus>,
    key: &AssetKey,
    streaming: bool,
    frame_height: &mut f32,
    frame_width: &mut f32,
) {
    let Some(AssetStatus::Ready(asset)) = assets.get(key) else {
        paint_asset(ui, assets, key, false);
        return;
    };

    let available_width = ui.available_width().max(1.0);
    let display_size = asset_display_size(asset.source_size, available_width, false, true);
    let width_changed = (*frame_width - available_width).abs() >= 1.0;
    *frame_height =
        next_preview_frame_height(*frame_height, display_size.y, width_changed, streaming);
    *frame_width = available_width;

    let uri = format!("bytes://rebook-chat-{}.svg", stable_hash(key));
    let image = egui::Image::new(ImageSource::Bytes {
        uri: uri.into(),
        bytes: egui::load::Bytes::Shared(Arc::clone(&asset.svg)),
    })
    .fit_to_exact_size(display_size);
    ui.allocate_ui_with_layout(
        Vec2::new(available_width, (*frame_height).max(display_size.y)),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.add(image);
        },
    );
}

fn paint_svg(ui: &mut egui::Ui, bytes: &[u8], namespace: &str, inline: bool) {
    let uri = format!(
        "bytes://rebook-chat-{namespace}-{}.svg",
        stable_hash(&bytes)
    );
    let mut image = egui::Image::new(ImageSource::Bytes {
        uri: uri.into(),
        bytes: egui::load::Bytes::Shared(Arc::from(bytes)),
    });
    if inline {
        image = image.max_height(22.0);
    } else {
        let source_size = match svg_source_size(bytes) {
            Ok(source_size) => source_size,
            Err(error) => {
                preview_error(ui, &error);
                return;
            }
        };
        image = image.fit_to_exact_size(asset_display_size(
            source_size,
            ui.available_width(),
            false,
            true,
        ));
    }
    ui.add(image);
}

fn preview_error(ui: &mut egui::Ui, error: &str) {
    egui::Frame::new()
        .fill(palette().error_fill)
        .corner_radius(5)
        .inner_margin(6)
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(error)
                        .size(crate::ui::scaled_font_size(11.5))
                        .color(palette().error_text),
                )
                .wrap()
                .selectable(true),
            );
        });
}

fn show_source_code(ui: &mut egui::Ui, source: &str) {
    egui::Frame::new()
        .fill(palette().surface)
        .corner_radius(5)
        .inner_margin(7)
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(source)
                        .monospace()
                        .size(crate::ui::scaled_font_size(11.5))
                        .color(palette().text),
                )
                .wrap()
                .selectable(true),
            );
        });
}

fn preview_mode_control(ui: &mut egui::Ui, mode: &mut PreviewMode, language: AppLanguage) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(94.0, 32.0), Sense::hover());
    let preview_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.center().x, rect.max.y));
    let code_rect = egui::Rect::from_min_max(egui::pos2(rect.center().x, rect.min.y), rect.max);
    let preview_response = ui.interact(
        preview_rect,
        ui.id().with("preview-mode-preview"),
        Sense::click(),
    );
    let code_response = ui.interact(code_rect, ui.id().with("preview-mode-code"), Sense::click());
    if preview_response.clicked() {
        *mode = PreviewMode::Preview;
    }
    if code_response.clicked() {
        *mode = PreviewMode::Code;
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 16.0, palette().pill_fill);
        let selected_rect = match mode {
            PreviewMode::Preview => preview_rect,
            PreviewMode::Code => code_rect,
        }
        .shrink(2.0);
        painter.rect_filled(selected_rect, 14.0, palette().surface);
        painter.rect_stroke(
            selected_rect,
            14.0,
            egui::Stroke::new(1.0, palette().pill_stroke),
            egui::StrokeKind::Inside,
        );

        for (segment, label, selected) in [
            (
                preview_rect,
                language.text("预览", "Preview"),
                *mode == PreviewMode::Preview,
            ),
            (
                code_rect,
                language.text("代码", "Code"),
                *mode == PreviewMode::Code,
            ),
        ] {
            painter.text(
                segment.center(),
                egui::Align2::CENTER_CENTER,
                label,
                FontId::new(if selected { 11.5 } else { 11.0 }, FontFamily::Proportional),
                if selected {
                    palette().text
                } else {
                    palette().muted
                },
            );
        }
    }
    preview_response.on_hover_cursor(egui::CursorIcon::PointingHand);
    code_response.on_hover_cursor(egui::CursorIcon::PointingHand);
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum PreviewKind {
    Svg,
    Mermaid,
}

fn preview_state_id(message_ordinal: usize, preview_ordinal: usize, kind: PreviewKind) -> u64 {
    stable_hash(&(message_ordinal, preview_ordinal, kind))
}

fn next_preview_frame_height(
    current: f32,
    rendered: f32,
    width_changed: bool,
    streaming: bool,
) -> f32 {
    if width_changed || !streaming {
        rendered
    } else {
        // Match the web preview: streaming updates may grow the frame but never
        // shrink it. This keeps the conversation from jumping on partial renders.
        current.max(rendered)
    }
}

impl PreviewKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Svg => "SVG",
            Self::Mermaid => "Mermaid",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PreviewMode {
    #[default]
    Preview,
    Code,
}

#[derive(Clone, Debug)]
struct PreviewState {
    mode: PreviewMode,
    full_size: bool,
    pending_asset: Option<AssetKey>,
    displayed_asset: Option<AssetKey>,
    frame_height: f32,
    frame_width: f32,
}

#[derive(Clone, Copy, Debug)]
struct PreviewContext {
    message_ordinal: usize,
    preview_ordinal: usize,
    streaming: bool,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            mode: PreviewMode::Preview,
            full_size: true,
            pending_asset: None,
            displayed_asset: None,
            frame_height: 0.0,
            frame_width: 0.0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RenderBlock<'a> {
    Markdown(&'a str),
    Preview { kind: PreviewKind, source: String },
}

#[derive(Debug, PartialEq, Eq)]
enum MarkdownLayoutBlock<'a> {
    Markdown(&'a str),
    Heading { level: usize, source: String },
    Table(MarkdownTable),
}

#[derive(Debug, Hash, PartialEq, Eq)]
struct MarkdownTable {
    rows: Vec<Vec<String>>,
}

struct MarkdownLine<'a> {
    start: usize,
    end: usize,
    content: &'a str,
}

fn split_markdown_layout_blocks(source: &str) -> Vec<MarkdownLayoutBlock<'_>> {
    let lines = markdown_lines(source);
    let mut blocks = Vec::new();
    let mut cursor = 0;
    let mut index = 0;
    let mut fence: Option<(u8, usize)> = None;
    while index < lines.len() {
        let line = &lines[index];
        if let Some((marker, length)) = markdown_fence(line.content) {
            if fence.is_some_and(|active| active.0 == marker && length >= active.1) {
                fence = None;
            } else if fence.is_none() {
                fence = Some((marker, length));
            }
            index += 1;
            continue;
        }
        if fence.is_some() {
            index += 1;
            continue;
        }

        if let Some((level, heading)) = markdown_atx_heading(line.content) {
            push_markdown_slice(&mut blocks, source, cursor, line.start);
            blocks.push(MarkdownLayoutBlock::Heading {
                level,
                source: heading,
            });
            cursor = line.end;
            index += 1;
            continue;
        }

        if index + 1 < lines.len()
            && let Some(mut table) = markdown_table_header(line.content, lines[index + 1].content)
        {
            push_markdown_slice(&mut blocks, source, cursor, line.start);
            let mut end = lines[index + 1].end;
            index += 2;
            while index < lines.len() {
                let row = split_markdown_table_row(lines[index].content);
                if row.len() < 2 {
                    break;
                }
                table.rows.push(row);
                end = lines[index].end;
                index += 1;
            }
            blocks.push(MarkdownLayoutBlock::Table(table));
            cursor = end;
            continue;
        }
        index += 1;
    }
    push_markdown_slice(&mut blocks, source, cursor, source.len());
    if blocks.is_empty() {
        blocks.push(MarkdownLayoutBlock::Markdown(source));
    }
    blocks
}

fn markdown_lines(source: &str) -> Vec<MarkdownLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        let end = start + line.len();
        lines.push(MarkdownLine {
            start,
            end,
            content: line.trim_end_matches(['\r', '\n']),
        });
        start = end;
    }
    if start < source.len() || source.is_empty() {
        lines.push(MarkdownLine {
            start,
            end: source.len(),
            content: &source[start..],
        });
    }
    lines
}

fn push_markdown_slice<'a>(
    blocks: &mut Vec<MarkdownLayoutBlock<'a>>,
    source: &'a str,
    start: usize,
    end: usize,
) {
    if start < end && !source[start..end].trim().is_empty() {
        blocks.push(MarkdownLayoutBlock::Markdown(&source[start..end]));
    }
}

fn markdown_fence(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start();
    let marker = *trimmed.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = trimmed.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some((marker, length))
}

fn markdown_atx_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) || !trimmed[level..].starts_with(char::is_whitespace) {
        return None;
    }
    let heading = trimmed[level..].trim();
    let heading = heading
        .strip_suffix('#')
        .map_or(heading, |value| value.trim_end_matches('#').trim_end());
    Some((level, heading.to_owned()))
}

fn markdown_table_header(header: &str, delimiter: &str) -> Option<MarkdownTable> {
    let header = split_markdown_table_row(header);
    let delimiter = split_markdown_table_row(delimiter);
    if header.len() < 2 || header.len() != delimiter.len() {
        return None;
    }
    let valid = delimiter.iter().all(|cell| {
        let rule = cell.trim().trim_start_matches(':').trim_end_matches(':');
        rule.len() >= 3 && rule.bytes().all(|byte| byte == b'-')
    });
    valid.then_some(MarkdownTable { rows: vec![header] })
}

fn split_markdown_table_row(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    let mut code_ticks = 0;
    let trimmed = line.trim().trim_matches('|');
    for character in trimmed.chars() {
        if escaped {
            cell.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            cell.push(character);
            continue;
        }
        if character == '`' {
            code_ticks ^= 1;
            cell.push(character);
            continue;
        }
        if character == '|' && code_ticks == 0 {
            cells.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(character);
        }
    }
    cells.push(cell.trim().to_owned());
    cells
}

fn markdown_plain_text(source: &str) -> String {
    let mut text = String::new();
    for event in Parser::new_ext(source, markdown_options()) {
        match event {
            Event::Text(value)
            | Event::Code(value)
            | Event::InlineMath(value)
            | Event::DisplayMath(value) => text.push_str(&value),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    if text.is_empty() {
        source.trim().to_owned()
    } else {
        text
    }
}

struct OpenFence {
    start: usize,
    language: String,
    content: String,
}

fn split_renderable_blocks(source: &str) -> Vec<RenderBlock<'_>> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    let mut open_fence: Option<OpenFence> = None;

    for (event, range) in Parser::new_ext(source, markdown_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(language))) => {
                open_fence = Some(OpenFence {
                    start: range.start,
                    language: language.into_string(),
                    content: String::new(),
                });
            }
            Event::Text(text) if open_fence.is_some() => {
                if let Some(fence) = &mut open_fence {
                    fence.content.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(fence) = open_fence.take()
                    && let Some(kind) = classify_preview(&fence.language, &fence.content)
                {
                    if cursor < fence.start {
                        blocks.push(RenderBlock::Markdown(&source[cursor..fence.start]));
                    }
                    blocks.push(RenderBlock::Preview {
                        kind,
                        source: fence.content,
                    });
                    cursor = range.end;
                }
            }
            _ => {}
        }
    }

    if cursor < source.len() {
        blocks.push(RenderBlock::Markdown(&source[cursor..]));
    }
    if blocks.is_empty() {
        blocks.push(RenderBlock::Markdown(source));
    }
    blocks
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct MarkdownDiagnosticSummary {
    pub(super) render_blocks: usize,
    pub(super) plain_fenced_code: usize,
    pub(super) tables: usize,
    pub(super) emoji_like: usize,
    pub(super) svg_previews: usize,
    pub(super) mermaid_previews: usize,
    pub(super) formulas: usize,
    pub(super) citations: usize,
}

pub(super) fn diagnostic_summary(source: &str) -> MarkdownDiagnosticSummary {
    let blocks = split_renderable_blocks(source);
    let mut summary = MarkdownDiagnosticSummary {
        render_blocks: blocks.len(),
        emoji_like: source
            .chars()
            .filter(|character| is_emoji_like(*character))
            .count(),
        citations: citation_locators(source).len(),
        ..MarkdownDiagnosticSummary::default()
    };
    for block in &blocks {
        if let RenderBlock::Preview { kind, .. } = block {
            match kind {
                PreviewKind::Svg => summary.svg_previews += 1,
                PreviewKind::Mermaid => summary.mermaid_previews += 1,
            }
        }
    }
    let mut fenced_code = 0_usize;
    for event in Parser::new_ext(source, markdown_options()) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => fenced_code += 1,
            Event::Start(Tag::Table(_)) => summary.tables += 1,
            Event::InlineMath(_) | Event::DisplayMath(_) => summary.formulas += 1,
            _ => {}
        }
    }
    summary.plain_fenced_code =
        fenced_code.saturating_sub(summary.svg_previews + summary.mermaid_previews);
    summary
}

fn is_emoji_like(character: char) -> bool {
    matches!(
        u32::from(character),
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2300..=0x23FF
    )
}

fn classify_preview(language: &str, source: &str) -> Option<PreviewKind> {
    let language = language
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match language.as_str() {
        "mermaid" | "mmd" if !source.trim().is_empty() => Some(PreviewKind::Mermaid),
        "svg" | "html" | "xml" if is_svg_markup(source) => Some(PreviewKind::Svg),
        _ => None,
    }
}

fn is_svg_markup(source: &str) -> bool {
    let normalized = source.trim().to_ascii_lowercase();
    normalized.contains("<svg") && normalized.contains("</svg>")
}

fn markdown_options() -> Options {
    Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
}

fn normalize_loose_strong_delimiters(source: &str) -> Cow<'_, str> {
    let mut output = String::with_capacity(source.len());
    let mut fence = None;
    let mut changed = false;
    for line in source.split_inclusive('\n') {
        let line_content = line.trim_end_matches(['\r', '\n']);
        let line_fence = markdown_fence(line_content).map(|(marker, _)| marker);
        if let Some(active) = fence {
            output.push_str(line);
            if line_fence == Some(active) {
                fence = None;
            }
        } else if let Some(opening) = line_fence {
            fence = Some(opening);
            output.push_str(line);
        } else {
            let normalized = normalize_loose_strong_line(line);
            changed |= matches!(normalized, Cow::Owned(_));
            output.push_str(normalized.as_ref());
        }
    }
    if changed {
        Cow::Owned(output)
    } else {
        Cow::Borrowed(source)
    }
}

fn normalize_loose_strong_line(line: &str) -> Cow<'_, str> {
    let bytes = line.as_bytes();
    let mut delimiters = Vec::new();
    let mut code_ticks = None;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let count = bytes[index..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            if code_ticks == Some(count) {
                code_ticks = None;
            } else if code_ticks.is_none() {
                code_ticks = Some(count);
            }
            index += count;
            continue;
        }
        if code_ticks.is_none()
            && bytes[index..].starts_with(b"**")
            && bytes.get(index.wrapping_sub(1)) != Some(&b'*')
            && bytes.get(index + 2) != Some(&b'*')
            && !is_escaped(bytes, index)
        {
            delimiters.push(index);
            index += 2;
            continue;
        }
        index += 1;
    }

    let mut replacements = Vec::new();
    for pair in delimiters.chunks_exact(2) {
        let open = pair[0];
        let close = pair[1];
        let inner = &line[open + 2..close];
        let trimmed_start = inner.trim_start_matches(char::is_whitespace);
        let leading_len = inner.len() - trimmed_start.len();
        let trimmed = trimmed_start.trim_end_matches(char::is_whitespace);
        let trailing_len = trimmed_start.len() - trimmed.len();
        if trimmed.is_empty() || (leading_len == 0 && trailing_len == 0) {
            continue;
        }
        replacements.push((open, close + 2, leading_len, trailing_len));
    }
    if replacements.is_empty() {
        return Cow::Borrowed(line);
    }

    let mut output = String::with_capacity(line.len());
    let mut cursor = 0;
    for (open, end, leading_len, trailing_len) in replacements {
        let before = &line[cursor..open];
        output.push_str(before);
        let inner = &line[open + 2..end - 2];
        let leading = &inner[..leading_len];
        let content_end = inner.len().saturating_sub(trailing_len);
        let content = &inner[leading_len..content_end];
        let trailing = &inner[content_end..];
        if !leading.is_empty() && !output.chars().next_back().is_some_and(char::is_whitespace) {
            output.push_str(leading);
        }
        output.push_str("**");
        output.push_str(content);
        output.push_str("**");
        if !trailing.is_empty() && !line[end..].chars().next().is_some_and(char::is_whitespace) {
            output.push_str(trailing);
        }
        cursor = end;
    }
    output.push_str(&line[cursor..]);
    Cow::Owned(output)
}

fn normalize_math_delimiters(source: &str) -> Cow<'_, str> {
    let mut output = String::with_capacity(source.len());
    let mut fence = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let line_fence = if trimmed.starts_with("```") {
            Some(b'`')
        } else if trimmed.starts_with("~~~") {
            Some(b'~')
        } else {
            None
        };
        if let Some(active) = fence {
            output.push_str(line);
            if line_fence == Some(active) {
                fence = None;
            }
        } else if let Some(opening) = line_fence {
            fence = Some(opening);
            output.push_str(line);
        } else {
            normalize_math_line(line, &mut output);
        }
    }
    if output == source {
        Cow::Borrowed(source)
    } else {
        Cow::Owned(output)
    }
}

fn normalize_math_line(line: &str, output: &mut String) {
    let mut converted = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut in_code = false;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let start = index;
            while index < bytes.len() && bytes[index] == b'`' {
                index += 1;
            }
            converted.push_str(&line[start..index]);
            in_code = !in_code;
            continue;
        }
        if !in_code && bytes[index] == b'\\' && index + 1 < bytes.len() {
            let replacement = match bytes[index + 1] {
                b'(' | b')' => Some("$"),
                b'[' | b']' => Some("$$"),
                _ => None,
            };
            if let Some(replacement) = replacement {
                converted.push_str(replacement);
                index += 2;
                continue;
            }
        }
        let next = line[index..]
            .char_indices()
            .nth(1)
            .map_or(bytes.len(), |(offset, _)| index + offset);
        converted.push_str(&line[index..next]);
        index = next;
    }
    normalize_loose_math_spacing(&converted, output);
}

fn normalize_loose_math_spacing(line: &str, output: &mut String) {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut in_code = false;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let start = index;
            while index < bytes.len() && bytes[index] == b'`' {
                index += 1;
            }
            output.push_str(&line[start..index]);
            in_code = !in_code;
            continue;
        }
        if !in_code && bytes[index] == b'$' && !is_escaped(bytes, index) {
            let delimiter_len = if bytes.get(index + 1) == Some(&b'$') {
                2
            } else {
                1
            };
            if let Some(end) =
                find_closing_math_delimiter(bytes, index + delimiter_len, delimiter_len)
            {
                let inner = &line[index + delimiter_len..end];
                let trimmed = inner.trim_matches([' ', '\t']);
                if !trimmed.is_empty() && trimmed.len() != inner.len() {
                    let delimiter = if delimiter_len == 2 { "$$" } else { "$" };
                    output.push_str(delimiter);
                    output.push_str(trimmed);
                    output.push_str(delimiter);
                    index = end + delimiter_len;
                    continue;
                }
            }
        }
        let next = line[index..]
            .char_indices()
            .nth(1)
            .map_or(bytes.len(), |(offset, _)| index + offset);
        output.push_str(&line[index..next]);
        index = next;
    }
}

fn find_closing_math_delimiter(
    bytes: &[u8],
    mut index: usize,
    delimiter_len: usize,
) -> Option<usize> {
    while index + delimiter_len <= bytes.len() {
        if bytes[index] == b'$'
            && !is_escaped(bytes, index)
            && (delimiter_len == 2) == (bytes.get(index + 1) == Some(&b'$'))
            && (delimiter_len == 2
                || (bytes.get(index.wrapping_sub(1)) != Some(&b'$')
                    && bytes.get(index + 1) != Some(&b'$')))
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = index;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn citation_locators(markdown: &str) -> Vec<String> {
    let normalized = normalize_internal_citations(markdown);
    let mut locators = Vec::new();
    for event in Parser::new_ext(normalized.as_ref(), markdown_options()) {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            let locator = dest_url.as_ref();
            if locator.starts_with(CHAT_CITATION_PREFIX)
                && !locators.iter().any(|item| item == locator)
            {
                locators.push(locator.to_owned());
            }
        }
    }
    locators
}

fn clicked_citation(cache: &CommonMarkCache, locators: &[String]) -> Option<String> {
    locators
        .iter()
        .find(|locator| cache.get_link_hook(locator) == Some(true))
        .cloned()
}

#[cfg(test)]
fn citation_icon_markdown(markdown: &str) -> Cow<'_, str> {
    citation_icon_markdown_with_groups(markdown).0
}

fn citation_icon_markdown_with_groups(
    markdown: &str,
) -> (Cow<'_, str>, HashMap<String, Vec<String>>) {
    let normalized = normalize_internal_citations(markdown);
    let markdown = normalized.as_ref();
    let mut replacements = Vec::new();
    let mut citation_start = None::<(usize, String)>;
    for (event, range) in Parser::new_ext(markdown, markdown_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Link { dest_url, .. })
                if dest_url.as_ref().starts_with(CHAT_CITATION_PREFIX) =>
            {
                citation_start = Some((range.start, dest_url.to_string()));
            }
            Event::End(TagEnd::Link) => {
                if let Some((start, locator)) = citation_start.take() {
                    replacements.push((start..range.end, locator));
                }
            }
            _ => {}
        }
    }
    if replacements.is_empty() {
        return (normalized, HashMap::new());
    }

    let mut output = String::with_capacity(markdown.len());
    let mut groups = HashMap::new();
    let mut cursor = 0;
    let mut index = 0;
    while index < replacements.len() {
        let (range, locator) = &replacements[index];
        if range.start < cursor {
            index += 1;
            continue;
        }
        let mut group_end = index + 1;
        while group_end < replacements.len() {
            let (previous_range, previous_locator) = &replacements[group_end - 1];
            let (next_range, next_locator) = &replacements[group_end];
            if !markdown[previous_range.end..next_range.start]
                .trim()
                .is_empty()
                || !citation_locators_are_adjacent(previous_locator, next_locator)
            {
                break;
            }
            group_end += 1;
        }
        output.push_str(&markdown[cursor..range.start]);
        output.push('[');
        output.push_str(CITATION_ICON);
        output.push_str("](<");
        output.push_str(locator);
        output.push_str(">)");
        if group_end > index + 1 {
            groups.insert(
                locator.clone(),
                replacements[index..group_end]
                    .iter()
                    .map(|(_, locator)| locator.clone())
                    .collect(),
            );
        }
        cursor = replacements[group_end - 1].0.end;
        index = group_end;
    }
    output.push_str(&markdown[cursor..]);
    (Cow::Owned(output), groups)
}

fn citation_locators_are_adjacent(left: &str, right: &str) -> bool {
    let Some((left_section, left_node)) = citation_locator_position(left) else {
        return false;
    };
    let Some((right_section, right_node)) = citation_locator_position(right) else {
        return false;
    };
    left_section == right_section && left_node.checked_add(1) == Some(right_node)
}

fn citation_locator_position(locator: &str) -> Option<(usize, u64)> {
    let remainder = locator.strip_prefix(CHAT_CITATION_PREFIX)?;
    let (section, node) = remainder.split_once('/')?;
    let section = section.parse().ok()?;
    let digits = node
        .rsplit_once(|character: char| !character.is_ascii_digit())
        .map_or(node, |(_, digits)| digits);
    (!digits.is_empty())
        .then(|| digits.parse().ok())
        .flatten()
        .map(|node| (section, node))
}

fn normalize_internal_citations(source: &str) -> Cow<'_, str> {
    match normalize_openai_citation_markers(source) {
        Cow::Borrowed(source) => normalize_legacy_internal_citations(source),
        Cow::Owned(source) => Cow::Owned(
            normalize_legacy_internal_citations(&source)
                .as_ref()
                .to_owned(),
        ),
    }
}

fn normalize_legacy_internal_citations(source: &str) -> Cow<'_, str> {
    match normalize_ref_citations(source) {
        Cow::Borrowed(source) => normalize_malformed_internal_citations(source),
        Cow::Owned(source) => Cow::Owned(
            normalize_malformed_internal_citations(&source)
                .as_ref()
                .to_owned(),
        ),
    }
}

fn normalize_openai_citation_markers(source: &str) -> Cow<'_, str> {
    const OPEN: &str = "【";
    const CLOSE: &str = "†source】";
    let mut search_start = 0;
    let mut copy_start = 0;
    let mut output = None::<String>;

    while let Some(relative_open) = source[search_start..].find(OPEN) {
        let open = search_start + relative_open;
        let content_start = open + OPEN.len();
        let Some(relative_close) = source[content_start..].find(CLOSE) else {
            break;
        };
        let close = content_start + relative_close;
        let body = source[content_start..close].trim();
        let locator = body
            .strip_prefix(CHAT_CITATION_PREFIX)
            .map_or_else(|| format!("{CHAT_CITATION_PREFIX}{body}"), str::to_owned);
        let next = close + CLOSE.len();
        if !is_internal_citation_locator(&locator) {
            search_start = next;
            continue;
        }

        let output = output.get_or_insert_with(|| String::with_capacity(source.len()));
        output.push_str(&source[copy_start..open]);
        output.push_str("[source](<");
        output.push_str(&locator);
        output.push_str(">)");
        copy_start = next;
        search_start = next;
    }

    output.map_or(Cow::Borrowed(source), |mut output| {
        output.push_str(&source[copy_start..]);
        Cow::Owned(output)
    })
}

fn normalize_ref_citations(source: &str) -> Cow<'_, str> {
    const OPEN: &str = "<ref>";
    const CLOSE: &str = "</ref>";
    let mut search_start = 0;
    let mut copy_start = 0;
    let mut output = None::<String>;

    while let Some(relative_open) = source[search_start..].find(OPEN) {
        let open = search_start + relative_open;
        let content_start = open + OPEN.len();
        let Some(relative_close) = source[content_start..].find(CLOSE) else {
            break;
        };
        let close = content_start + relative_close;
        let locator = source[content_start..close].trim();
        let next = close + CLOSE.len();
        if !is_internal_citation_locator(locator)
            || locator
                .chars()
                .any(|character| character.is_whitespace() || matches!(character, '<' | '>'))
        {
            search_start = next;
            continue;
        }

        let output = output.get_or_insert_with(|| String::with_capacity(source.len()));
        output.push_str(&source[copy_start..open]);
        output.push_str("[ref](<");
        output.push_str(locator);
        output.push_str(">)");
        copy_start = next;
        search_start = next;
    }

    output.map_or(Cow::Borrowed(source), |mut output| {
        output.push_str(&source[copy_start..]);
        Cow::Owned(output)
    })
}

fn normalize_malformed_internal_citations(source: &str) -> Cow<'_, str> {
    const LINK_OPEN: &str = "](link://j/";
    let mut search_start = 0;
    let mut copy_start = 0;
    let mut output = None::<String>;
    while let Some(relative_start) = source[search_start..].find(LINK_OPEN) {
        let link_start = search_start + relative_start + 2;
        let mut malformed_close = None;
        for (offset, character) in source[link_start..].char_indices() {
            match character {
                ')' => break,
                ']' => {
                    malformed_close = Some(link_start + offset);
                    break;
                }
                character if character.is_whitespace() || matches!(character, '(' | '<' | '>') => {
                    break;
                }
                _ => {}
            }
        }
        let Some(close) = malformed_close else {
            search_start = link_start;
            continue;
        };
        let locator = &source[link_start..close];
        if !is_internal_citation_locator(locator) {
            search_start = close + 1;
            continue;
        }
        let output = output.get_or_insert_with(|| String::with_capacity(source.len()));
        output.push_str(&source[copy_start..close]);
        output.push(')');
        copy_start = close + 1;
        search_start = copy_start;
    }
    output.map_or(Cow::Borrowed(source), |mut output| {
        output.push_str(&source[copy_start..]);
        Cow::Owned(output)
    })
}

fn is_internal_citation_locator(locator: &str) -> bool {
    let Some(remainder) = locator.strip_prefix(CHAT_CITATION_PREFIX) else {
        return false;
    };
    let (section, node) = remainder
        .split_once('/')
        .map_or((remainder, None), |(section, node)| (section, Some(node)));
    !section.is_empty()
        && section.bytes().all(|byte| byte.is_ascii_digit())
        && node.is_none_or(|node| !node.is_empty())
}

fn stable_hash<T: Hash + ?Sized>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_svg_and_mermaid_without_replacing_regular_code() {
        let source = r#"# Heading

**bold**

```rust
fn main() {}
```

```svg
<svg viewBox="0 0 10 10"><circle cx="5" cy="5" r="4" /></svg>
```

```mmd
flowchart LR
    A --> B
```
"#;
        let blocks = split_renderable_blocks(source);
        assert_eq!(blocks.len(), 5);
        assert!(
            matches!(blocks[0], RenderBlock::Markdown(markdown) if markdown.contains("```rust"))
        );
        assert!(matches!(
            blocks[1],
            RenderBlock::Preview {
                kind: PreviewKind::Svg,
                ..
            }
        ));
        assert!(matches!(blocks[2], RenderBlock::Markdown(markdown) if markdown.trim().is_empty()));
        assert!(matches!(
            blocks[3],
            RenderBlock::Preview {
                kind: PreviewKind::Mermaid,
                ..
            }
        ));
        assert!(matches!(blocks[4], RenderBlock::Markdown(markdown) if markdown.trim().is_empty()));
    }

    #[test]
    fn incomplete_streamed_mermaid_fence_is_available_for_preview() {
        let source = "正在生成图表：\n\n```mermaid\nflowchart TB\n    A --> B";
        let blocks = split_renderable_blocks(source);

        assert!(matches!(
            blocks.last(),
            Some(RenderBlock::Preview {
                kind: PreviewKind::Mermaid,
                source,
            }) if source.contains("A --> B")
        ));
        assert_eq!(diagnostic_summary(source).mermaid_previews, 1);
    }

    #[test]
    fn streaming_asset_queue_retries_the_newest_source_after_backpressure() {
        let (asset_sender, request_receiver) = mpsc::sync_channel(1);
        let (_result_sender, asset_receiver) = mpsc::channel();
        let mut state = ChatMarkdownState {
            markdown_cache: CommonMarkCache::default(),
            assets: HashMap::new(),
            asset_sender,
            asset_receiver,
            preview_states: HashMap::new(),
        };
        let first = AssetKey::Mermaid("flowchart LR\nA".to_owned());
        let newest = AssetKey::Mermaid("flowchart LR\nA --> B".to_owned());

        state.queue_asset(first.clone());
        state.queue_asset(newest.clone());
        assert!(state.assets.contains_key(&first));
        assert!(!state.assets.contains_key(&newest));

        let _ = request_receiver.recv().unwrap();
        state.queue_asset(newest.clone());
        assert!(state.assets.contains_key(&newest));
    }

    #[test]
    fn recognizes_html_svg_but_not_regular_html_or_code() {
        assert_eq!(
            classify_preview("html", "<svg><path /></svg>"),
            Some(PreviewKind::Svg)
        );
        assert_eq!(classify_preview("html", "<div>hello</div>"), None);
        assert_eq!(classify_preview("rust", "let x = 1;"), None);
    }

    #[test]
    fn extracts_headings_and_tables_for_desktop_styling() {
        let source = "# 补充说明\n\n| 对比维度 | 抽象解法 | 无穷级数解法 |\n| --- | --- | --- |\n| 核心思路 | 建立整体方程 | 逐项累加 |\n";
        let blocks = split_markdown_layout_blocks(source);

        assert!(matches!(
            &blocks[0],
            MarkdownLayoutBlock::Heading { level: 1, source } if source == "补充说明"
        ));
        let MarkdownLayoutBlock::Table(table) = &blocks[1] else {
            panic!("expected a styled table block");
        };
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0], ["对比维度", "抽象解法", "无穷级数解法"]);
        assert_eq!(table.rows[1], ["核心思路", "建立整体方程", "逐项累加"]);
    }

    #[test]
    fn markdown_table_row_height_follows_the_tallest_wrapped_cell() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let mut short_height = 0.0;
        let mut wrapped_height = 0.0;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let short = vec!["Short".to_owned(), "Cell".to_owned()];
            let wrapped = vec![
                "Short".to_owned(),
                "A much longer table cell that must wrap across several lines".to_owned(),
            ];

            short_height = markdown_table_row_height(ui, &short, 2, 100.0);
            wrapped_height = markdown_table_row_height(ui, &wrapped, 2, 100.0);
        })
        .drop_without_applying_deltas();
        assert!(
            wrapped_height > short_height,
            "wrapped={wrapped_height}, short={short_height}"
        );
    }

    #[test]
    fn preview_defaults_to_full_size_and_can_be_compacted() {
        let state = PreviewState::default();

        assert_eq!(state.mode, PreviewMode::Preview);
        assert!(state.full_size);
    }

    #[test]
    fn streaming_preview_identity_is_independent_of_growing_source() {
        let initial = preview_state_id(3, 1, PreviewKind::Mermaid);
        let after_more_tokens = preview_state_id(3, 1, PreviewKind::Mermaid);

        assert_eq!(initial, after_more_tokens);
        assert_ne!(initial, preview_state_id(3, 2, PreviewKind::Mermaid));
    }

    #[test]
    fn streaming_preview_height_grows_without_shrinking() {
        for (current, rendered, width_changed, streaming, expected) in [
            (420.0, 280.0, false, true, 420.0),
            (280.0, 420.0, false, true, 420.0),
            (420.0, 280.0, false, false, 280.0),
            (420.0, 280.0, true, true, 280.0),
        ] {
            let actual = next_preview_frame_height(current, rendered, width_changed, streaming);
            assert!((actual - expected).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn markdown_parser_detects_inline_and_display_math() {
        let events = Parser::new_ext(
            "Inline $x^2$ and display $$\\frac{a}{b}$$",
            markdown_options(),
        )
        .collect::<Vec<_>>();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::InlineMath(_)))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::DisplayMath(_)))
        );
    }

    #[test]
    fn markdown_parser_keeps_chinese_strong_labels_as_bold_text() {
        let events = Parser::new_ext(
            "**一句话概括**：本节比较两种解法。\n\n**关键要点**：",
            markdown_options(),
        )
        .collect::<Vec<_>>();

        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Start(Tag::Strong)))
                .count(),
            2
        );
    }

    #[test]
    fn normalizes_model_authored_whitespace_inside_strong_delimiters() {
        let source = "如** “可瞥见阅读”（glanceable reading）**，可快速浏览。";
        let normalized = normalize_loose_strong_delimiters(source);
        assert_eq!(
            normalized,
            "如 **“可瞥见阅读”（glanceable reading）**，可快速浏览。"
        );
        assert!(
            Parser::new_ext(normalized.as_ref(), markdown_options())
                .any(|event| matches!(event, Event::Start(Tag::Strong)))
        );
        assert_eq!(
            normalize_loose_strong_delimiters("`** code **`\n\n```md\n** code **\n```"),
            "`** code **`\n\n```md\n** code **\n```"
        );
    }

    #[test]
    fn normalizes_latex_parentheses_without_touching_code() {
        let source =
            "Inline \\(x^2\\), display \\[y^2\\], and `\\(code\\)`.\n\n```tex\n\\(code\\)\n```";
        let normalized = normalize_math_delimiters(source);
        assert!(normalized.contains("Inline $x^2$, display $$y^2$$"));
        assert!(normalized.contains("`\\(code\\)`"));
        assert!(normalized.contains("```tex\n\\(code\\)\n```"));
    }

    #[test]
    fn normalizes_user_math_with_loose_delimiter_spacing() {
        let source =
            "Loose $ 1/2 + 1/8 + 1/32 + \\cdots $ and \\( p = \\frac12 + \\cdots \\). ` $ code $ `";
        let normalized = normalize_math_delimiters(source);

        assert!(normalized.contains("$1/2 + 1/8 + 1/32 + \\cdots$"));
        assert!(normalized.contains("$p = \\frac12 + \\cdots$"));
        assert!(normalized.contains("` $ code $ `"));
        let formulas = Parser::new_ext(normalized.as_ref(), markdown_options())
            .filter(|event| matches!(event, Event::InlineMath(_)))
            .count();
        assert_eq!(formulas, 2);
    }

    #[test]
    fn extracts_only_internal_reader_citations_from_markdown_links() {
        assert_eq!(
            citation_locators(
                "See [source](link://j/2/paragraph-3) and [web](https://example.com)."
            ),
            vec!["link://j/2/paragraph-3"]
        );
        assert!(citation_locators("[legacy](link:/j/2/paragraph-3)").is_empty());
    }

    #[test]
    fn ref_citation_protocol_is_iconized_and_clickable() {
        let source = "First<ref>link://j/11/n17</ref><ref>link://j/11/n44</ref>";

        assert_eq!(
            citation_locators(source),
            vec!["link://j/11/n17", "link://j/11/n44"]
        );
        assert_eq!(
            citation_icon_markdown(source),
            format!(
                "First[{CITATION_ICON}](<link://j/11/n17>)[{CITATION_ICON}](<link://j/11/n44>)"
            )
        );
    }

    #[test]
    fn openai_style_citation_markers_are_iconized_and_clickable() {
        let source = "First【11/n17†source】【11/n44†source】";

        assert_eq!(
            citation_locators(source),
            vec!["link://j/11/n17", "link://j/11/n44"]
        );
        assert_eq!(
            citation_icon_markdown(source),
            format!(
                "First[{CITATION_ICON}](<link://j/11/n17>)[{CITATION_ICON}](<link://j/11/n44>)"
            )
        );
    }

    #[test]
    fn incomplete_openai_style_marker_waits_for_the_streaming_suffix() {
        assert!(matches!(
            normalize_openai_citation_markers("Streaming 【11/n17†sour"),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            normalize_openai_citation_markers("Done 【11/n17†source】"),
            "Done [source](<link://j/11/n17>)"
        );
    }

    #[test]
    fn incomplete_streaming_ref_is_ignored_until_the_closing_tag_arrives() {
        assert!(matches!(
            normalize_ref_citations("Streaming <ref>link://j/11/n17"),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            normalize_ref_citations("Done <ref> link://j/11/n17 </ref>"),
            "Done [ref](<link://j/11/n17>)"
        );
    }

    #[test]
    fn malformed_internal_citation_closing_brackets_are_normalized() {
        let source = "第一处[出处](link://j/56]，第二处[出处](link://j/57/n2]，网页[来源](https://example.com]。";
        let normalized = normalize_malformed_internal_citations(source);

        assert_eq!(
            normalized,
            "第一处[出处](link://j/56)，第二处[出处](link://j/57/n2)，网页[来源](https://example.com]。"
        );
        assert_eq!(
            citation_locators(source),
            vec!["link://j/56", "link://j/57/n2"]
        );
        let iconized = citation_icon_markdown(source);
        assert_eq!(iconized.matches(CITATION_ICON).count(), 2);
        assert!(!iconized.contains("[["));
        assert!(iconized.contains("[来源](https://example.com]"));
    }

    #[test]
    fn malformed_internal_citation_is_replaced_without_a_leftover_bracket() {
        assert_eq!(
            citation_icon_markdown("[source](link://j/56]"),
            format!("[{CITATION_ICON}](<link://j/56>)")
        );
    }

    #[test]
    fn incomplete_streaming_citations_are_left_untouched_until_the_locator_is_complete() {
        assert!(matches!(
            normalize_malformed_internal_citations("生成中[出处](link://j/"),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            normalize_malformed_internal_citations("已完成[出处](link://j/56]"),
            "已完成[出处](link://j/56)"
        );
        assert!(matches!(
            normalize_malformed_internal_citations("标准[出处](link://j/56)"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn internal_citation_labels_are_replaced_with_external_link_icons() {
        let source =
            "结论[中文引用](link://j/2/chapter%2Fparagraph-3)，另见[网页](https://example.com)。";
        let iconized = citation_icon_markdown(source);

        assert!(!iconized.contains("中文引用"));
        assert!(iconized.contains(CITATION_ICON));
        assert!(iconized.contains("[网页](https://example.com)"));
        assert_eq!(
            citation_locators(iconized.as_ref()),
            vec!["link://j/2/chapter%2Fparagraph-3"]
        );
    }

    #[test]
    fn citation_icon_rendering_preserves_model_authored_separators() {
        let source = "[出处](link://j/1/n1)、[出处](link://j/2/n2)";

        assert!(citation_icon_markdown(source).contains("、"));
    }

    #[test]
    fn adjacent_paragraph_citations_share_one_icon_and_click_target() {
        let source = "结论【11/n17†source】【11/n18†source】";
        let (iconized, groups) = citation_icon_markdown_with_groups(source);
        assert_eq!(iconized.matches(CITATION_ICON).count(), 1);
        assert_eq!(
            groups.get("link://j/11/n17"),
            Some(&vec!["link://j/11/n17".into(), "link://j/11/n18".into()])
        );

        let (iconized, groups) =
            citation_icon_markdown_with_groups("结论【11/n17†source】【11/n19†source】");
        assert_eq!(iconized.matches(CITATION_ICON).count(), 2);
        assert!(groups.is_empty());
    }

    #[test]
    fn markdown_selection_fill_preserves_the_reader_green_when_precomposited() {
        assert_eq!(
            opaque_over(text_selection_fill(), Color32::WHITE),
            Color32::from_rgb(202, 222, 212)
        );
    }

    #[test]
    fn an_earlier_fragment_click_survives_later_link_hook_resets() {
        let first = "link://j/1/first".to_owned();
        let second = "link://j/2/second".to_owned();
        let locators = vec![first.clone(), second.clone()];
        let mut cache = CommonMarkCache::default();
        cache.add_link_hook(first.clone());
        cache.add_link_hook(second.clone());
        cache.link_hooks_mut().insert(first.clone(), true);

        let mut clicked = clicked_citation(&cache, &locators);
        cache.link_hooks_mut().insert(first.clone(), false);
        cache.link_hooks_mut().insert(second, false);
        clicked = clicked.or_else(|| clicked_citation(&cache, &locators));

        assert_eq!(clicked, Some(first));
    }

    #[test]
    fn mermaid_renderer_produces_an_svg() {
        let svg = mermaid_rs_renderer::render("flowchart LR; A-->B").unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn user_mermaid_graph_has_visible_raster_content() {
        let source = r#"flowchart TB
    A[第2.2节：抛硬币游戏与抽象思维] --> B[核心问题]
    A --> C[抽象解法示例]
    A --> D[对比案例：火车与苍蝇]
    A --> E[配套练习题]

    B --> B1["抛硬币游戏<br>求第一位玩家获胜概率 p"]
    B1 --> B2["建立关系：P(第一位) + P(第二位) = 1<br>即 p + p/2 = 1"]
    B2 --> B3["解得 p = 2/3"]

    C --> C1["抽象（Abstraction）"]
    C1 --> C2["忽略无穷级数细节"]
    C1 --> C3["建立整体量关系（如概率总和为1）"]
    C1 --> C4["获得洞察力，答案自然显现"]

    D --> D1["两列火车相距60英里<br>各20 mph相向而行"]
    D --> D2["苍蝇以30 mph来回飞行"]
    D --> D3["求解方法对比"]
    D3 --> D4["洞察解法<br>（利用火车相遇时间）"]
    D3 --> D5["无穷级数解法<br>（冯·诺依曼使用）"]
    D4 & D5 --> D6["结果一致，殊途同归"]

    E --> E1[练习2.4：抽象求几何级数和]
    E --> E2[练习2.5：验证概率p=2/3]
    E --> E3[练习2.6：嵌套平方根]
    E --> E4[练习2.7：两种方法解火车苍蝇问题]"#;
        let svg = mermaid_rs_renderer::render(source).unwrap();
        let AssetStatus::Ready(asset) = ready_svg_asset(svg).unwrap() else {
            panic!("expected a ready SVG asset");
        };
        assert!(asset.source_size[0] / asset.source_size[1] > 3.0);

        let display_size = asset_display_size(asset.source_size, 500.0, false, true);
        assert!((display_size.x - 500.0).abs() < 0.1);
        assert!(
            (display_size.x / display_size.y - asset.source_size[0] / asset.source_size[1]).abs()
                < 0.01
        );
    }

    #[test]
    fn block_assets_fill_width_without_a_fixed_height() {
        let display_size = asset_display_size([400.0, 800.0], 500.0, false, true);

        assert_eq!(display_size, Vec2::new(500.0, 1_000.0));
    }

    #[test]
    fn formulas_are_not_upscaled_to_fill_the_container() {
        let display_size = asset_display_size([120.0, 30.0], 500.0, false, false);

        assert_eq!(display_size, Vec2::new(120.0, 30.0));
    }

    #[test]
    fn formula_renderer_produces_a_standalone_svg() {
        let svg = render_formula(r"\frac{a}{b}", false).unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));

        let AssetStatus::Ready(asset) = ready_svg_asset(svg).unwrap() else {
            panic!("expected a ready formula SVG asset");
        };
        assert!(asset.source_size[0] > 0.0);
        assert!(asset.source_size[1] > 0.0);
    }

    #[test]
    fn formula_renderer_supports_cjk_text_units_with_slashes() {
        let svg = render_formula(r"4\text{千卡/克}", true).unwrap();

        assert!(!svg.contains("PARSE ERROR"));
        let AssetStatus::Ready(asset) = ready_svg_asset(svg).unwrap() else {
            panic!("expected a ready formula SVG asset");
        };
        assert!(asset.source_size[0] > 0.0);
        assert!(asset.source_size[1] > 0.0);
    }

    #[test]
    fn diagnostic_summary_classifies_text_visualization_response_without_logging_content() {
        let source = "📁 root\n\n```\nroot\n└── file\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |";
        let summary = diagnostic_summary(source);
        assert_eq!(summary.plain_fenced_code, 1);
        assert_eq!(summary.tables, 1);
        assert_eq!(summary.emoji_like, 1);
        assert_eq!(summary.svg_previews, 0);
        assert_eq!(summary.mermaid_previews, 0);
        assert_eq!(summary.formulas, 0);
        assert_eq!(summary.citations, 0);
    }

    #[test]
    fn diagnostic_summary_counts_renderable_visuals_and_math() {
        let source = "```mermaid\nflowchart LR; A-->B\n```\n\n```svg\n<svg></svg>\n```\n\n$x$";
        let summary = diagnostic_summary(source);
        assert_eq!(summary.plain_fenced_code, 0);
        assert_eq!(summary.svg_previews, 1);
        assert_eq!(summary.mermaid_previews, 1);
        assert_eq!(summary.formulas, 1);
        assert_eq!(summary.citations, 0);
    }

    #[test]
    fn diagnostic_summary_counts_unique_internal_citations() {
        let source = "[one](link://j/1/p-1) [again](link://j/1/p-1) [two](link://j/2)";

        assert_eq!(diagnostic_summary(source).citations, 2);
    }
}
