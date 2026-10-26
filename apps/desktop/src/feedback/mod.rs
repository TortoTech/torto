mod model;
pub(crate) mod screenshots;
#[cfg(test)]
mod tests;
mod transport;

use crate::async_task::{TaskResult, TaskSlot};
use crate::platform::UserEvent;
use crate::preferences::AppLanguage;
use crate::ui::{Icon, dialog_action_button, icon_button, palette};
pub(crate) use model::{
    Book, FocusLocation, Location, OpenContext, Reader, Translation, book_document, clipped,
};
use model::{Draft, Kind, Payload, character_count, valid_description};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use transport::{Failure, Outcome};

#[derive(Clone)]
enum Work {
    Prepare(OpenContext, bool),
    Send(Draft),
}
pub(crate) enum Output {
    Ready(Draft, bool),
    Sent(Draft, Outcome),
}
pub(crate) type Message = TaskResult<Output>;

#[derive(Default)]
enum Status {
    #[default]
    Editing,
    Preparing,
    Sending,
    Pending,
    Failed(Failure),
    Submitted(transport::Issue),
}

#[derive(Default)]
pub(crate) struct FeedbackFeature {
    visible: bool,
    visibility: f32,
    presented: bool,
    loaded: bool,
    locked: bool,
    focus_input: bool,
    draft: Option<Draft>,
    editor: Option<(Kind, String, Vec<model::Screenshot>)>,
    seed: Option<OpenContext>,
    status: Status,
    retry_at: Option<Instant>,
    task: TaskSlot<Work>,
    image_task: TaskSlot<screenshots::Source>,
    thumbnails: Vec<egui::TextureHandle>,
    prepared_images: Vec<egui::ColorImage>,
    image_error: Option<String>,
    validation_requested: bool,
    ime_active: bool,
    pending_paste: Vec<egui::Event>,
    pasted_image: Option<bool>,
    opening_size: Option<egui::Vec2>,
    layout_ready: bool,
    preview_task: TaskSlot<model::Screenshot>,
    preview_pixels: Option<egui::ColorImage>,
    preview: Option<crate::ui::image_preview::ImagePreview>,
}

impl FeedbackFeature {
    pub(crate) fn is_open(&self) -> bool {
        self.visible || self.visibility > f32::EPSILON
    }

    pub(crate) fn open(&mut self, seed: OpenContext) {
        self.clear_preview();
        self.visible = true;
        self.presented = false;
        self.opening_size = None;
        self.layout_ready = false;
        if self.task.is_pending()
            || self.locked && self.draft.is_some() && !matches!(self.status, Status::Submitted(_))
        {
            return;
        }
        if matches!(self.status, Status::Submitted(_)) {
            self.editor = None;
            self.draft = None;
        } else if let Some(draft) = self.draft.take() {
            self.editor = Some((
                draft.payload.kind,
                draft.payload.description,
                draft.payload.screenshots,
            ));
        }
        self.image_task.cancel();
        self.thumbnails.clear();
        self.prepared_images.clear();
        self.image_error = None;
        self.validation_requested = false;
        self.ime_active = false;
        self.pending_paste.clear();
        self.pasted_image = None;
        self.locked = false;
        self.retry_at = None;
        self.status = Status::Preparing;
        self.seed = Some(seed.clone());
        self.task.begin(Work::Prepare(seed, !self.loaded));
    }

    pub(crate) fn complete(&mut self, message: Message) {
        if self.task.complete(message.id).is_none() {
            return;
        }
        match message.result {
            Ok(Output::Ready(mut draft, restored)) => {
                self.opening_size = None;
                if !restored && let Some((kind, description, screenshots)) = self.editor.take() {
                    draft.payload.kind = kind;
                    draft.payload.description = description;
                    draft.payload.screenshots = screenshots;
                }
                if !draft.payload.screenshots.is_empty() {
                    self.image_task.begin(screenshots::Source::Restore(
                        draft.payload.screenshots.clone(),
                    ));
                }
                self.loaded = true;
                self.locked = restored;
                self.status = if restored {
                    Status::Pending
                } else {
                    Status::Editing
                };
                self.retry_at = remaining_retry(&draft).map(|d| Instant::now() + d);
                self.draft = Some(draft);
                self.focus_input = !restored;
            }
            Ok(Output::Sent(draft, result)) => {
                if matches!(&result, Outcome::Submitted(_)) {
                    self.opening_size = None;
                    self.layout_ready = false;
                }
                self.retry_at = remaining_retry(&draft).map(|d| Instant::now() + d);
                self.status = match result {
                    Outcome::Submitted(issue) => Status::Submitted(issue),
                    Outcome::Pending(_) => Status::Pending,
                    Outcome::Failed(failure, _) => {
                        if failure == Failure::Invalid {
                            self.locked = false;
                            self.focus_input = true;
                        }
                        Status::Failed(failure)
                    }
                };
                self.draft = Some(draft);
            }
            Err(error) => {
                self.status = Status::Failed(if error == "configuration" {
                    Failure::Configuration
                } else {
                    Failure::Storage
                })
            }
        }
    }

    fn send(&mut self) {
        if self.task.is_pending()
            || self.image_task.is_pending()
            || self.retry_at.is_some_and(|at| at > Instant::now())
        {
            return;
        }
        let Some(draft) = &mut self.draft else {
            return;
        };
        if !valid_description(&draft.payload.description) {
            self.validation_requested = true;
            return;
        }
        if !self.locked {
            draft.payload.request_id = uuid::Uuid::new_v4().to_string();
            draft.payload.description = draft.payload.description.trim().to_owned();
        }
        self.locked = true;
        self.status = Status::Sending;
        self.task.begin(Work::Send(draft.clone()));
    }

    pub(crate) fn spawn_pending_tasks(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if let Some(request) = self.preview_task.take_pending() {
            let proxy = proxy.clone();
            self.preview_task.attach_worker(runtime.spawn(async move {
                let result = screenshots::preview(request.payload).await;
                let _ = proxy.send_event(UserEvent::FeedbackPreview(TaskResult { id: request.id, result }));
            }));
        }
        if let Some(request) = self.image_task.take_pending() {
            let proxy = proxy.clone();
            self.image_task.attach_worker(runtime.spawn(async move {
                let result = screenshots::prepare(request.payload).await;
                let _ = proxy.send_event(UserEvent::FeedbackImages(TaskResult {
                    id: request.id,
                    result,
                }));
            }));
        }
        let Some(request) = self.task.take_pending() else {
            return;
        };
        let proxy = proxy.clone();
        self.task.attach_worker(runtime.spawn(async move {
            let result = execute(request.payload).await;
            let _ = proxy.send_event(UserEvent::Feedback(TaskResult {
                id: request.id,
                result,
            }));
        }));
    }

    pub(crate) fn accepts_images(&self) -> bool {
        self.visible && !self.preview_active() && !self.locked && self.draft.is_some() && !self.image_task.is_pending()
    }

    fn preview_active(&self) -> bool {
        self.preview.is_some() || self.preview_pixels.is_some() || self.preview_task.is_pending()
    }

    fn clear_preview(&mut self) {
        self.preview_task.cancel();
        self.preview_pixels = None;
        self.preview = None;
    }

    fn open_preview(&mut self, index: usize) {
        let Some(image) = self.draft.as_ref().and_then(|draft| draft.payload.screenshots.get(index)).cloned() else { return };
        self.clear_preview();
        self.image_error = None;
        self.preview_task.begin(image);
    }

    pub(crate) fn complete_preview(&mut self, message: screenshots::PreviewMessage) {
        if self.preview_task.complete(message.id).is_none() || !self.visible { return; }
        match message.result {
            Ok(image) => self.preview_pixels = Some(image),
            Err(error) => self.image_error = Some(error),
        }
    }

    pub(crate) fn paste_images(&mut self) {
        if self.accepts_images() {
            self.image_error = None;
            self.pasted_image = None;
            self.image_task.begin(screenshots::Source::Clipboard);
        }
    }

    pub(crate) fn complete_images(&mut self, message: screenshots::Message) {
        let Some(source) = self.image_task.complete(message.id) else {
            return;
        };
        let restoring = matches!(source, screenshots::Source::Restore(_));
        if matches!(source, screenshots::Source::Clipboard) {
            // The same clipboard can expose both image pixels and a filename/URL.
            // Delay egui's text paste until the worker decides which representation wins.
            self.pasted_image = Some(message.result.as_ref().map_or(true, |images| !images.is_empty()));
        }
        match message.result {
            Ok(images) => {
                let Some(draft) = &mut self.draft else { return };
                if !restoring
                    && (self.locked
                        || draft.payload.screenshots.len() + images.len() > screenshots::MAX_IMAGES)
                {
                    self.image_error = Some("limit".into());
                    return;
                }
                for image in images {
                    if !restoring {
                        draft.payload.screenshots.push(image.screenshot);
                    }
                    self.prepared_images.push(image.thumbnail);
                }
                self.image_error = None;
                self.focus_input = !self.locked;
            }
            Err(error) => self.image_error = Some(error),
        }
    }

    pub(crate) fn overlay(&mut self, ctx: &egui::Context, language: AppLanguage) {
        // Match the settings dialog's motion, starting only once the form has
        // its final dimensions. A compact success state is not a new opening.
        self.presented |= self.visible && self.layout_ready;
        let visibility = ctx.animate_bool_with_time(
            egui::Id::new("feedback-overlay-motion"),
            self.visible && self.presented,
            0.18,
        );
        let was_presented = self.visibility > f32::EPSILON;
        self.visibility = visibility;
        if !self.visible && visibility <= f32::EPSILON {
            // The parent was still input-blocked earlier in this frame. Draw it
            // once more after releasing the modal, then return to settled idle.
            if was_presented { ctx.request_repaint(); }
            return;
        }
        if let Some(image) = self.preview_pixels.take() {
            self.preview = Some(crate::ui::image_preview::ImagePreview::new(ctx, image, "feedback-screenshot-preview"));
            ctx.memory_mut(|memory| {
                if let Some(id) = memory.focused() { memory.surrender_focus(id); }
            });
        }
        // The preview owns input until its fade ends, including the frame that releases it.
        let preview_active = self.preview_active();
        if let Some(preview) = &mut self.preview {
            preview.keyboard(ctx);
            if preview.show(ctx) {
                self.preview = None;
                self.focus_input = !self.locked;
                ctx.request_repaint();
            }
        } else if preview_active && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.clear_preview();
            self.focus_input = !self.locked;
            ctx.request_repaint();
        }
        let mut send = false;
        let mut close = false;
        let mut prepare = false;
        if matches!(self.image_task.active(), Some(screenshots::Source::Clipboard)) || self.pasted_image.is_some() {
            ctx.input_mut(|input| {
                input.events.retain(|event| {
                    if matches!(event, egui::Event::Paste(_)) {
                        self.pending_paste.push(event.clone());
                        false
                    } else { true }
                });
                if let Some(image) = self.pasted_image.take() {
                    if image { self.pending_paste.clear(); }
                    else { input.events.append(&mut self.pending_paste); }
                }
            });
        }
        let native_composing = egui::native_text_input::blocks_submit(ctx);
        let enter = ctx.input_mut(|input| submit_key(&mut input.events, &mut self.ime_active)) && !native_composing && !preview_active && self.visible;
        for image in self.prepared_images.drain(..) {
            self.thumbnails.push(ctx.load_texture(
                format!("feedback-image-{}", uuid::Uuid::new_v4()),
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        let preparing = matches!(self.status, Status::Preparing)
            || matches!(self.image_task.active(), Some(screenshots::Source::Restore(_)));
        let measuring = !self.layout_ready;
        let mut preview_index = None;
        let screen = crate::ui::overlay_rect(ctx);
        let modal_id = egui::Id::new("feedback-modal");
        let modal_area = crate::ui::modal_area(ctx, modal_id).anchor(
            egui::Align2::CENTER_CENTER,
            screen.center() - ctx.content_rect().center() + egui::vec2(0.0, (1.0 - visibility) * 12.0),
        );
        let response = crate::ui::modal(ctx, modal_id, 0.46 * visibility)
            .area(modal_area)
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                if preview_active || !self.visible {
                    ui.disable();
                    // Freeze closing content without an extra disabled-widget fade.
                    if !self.visible { ui.set_opacity(1.0); }
                }
                // Keep the backdrop active, but do not paint a partial form or reuse
                // the previous dialog's dimensions while this opening is measured.
                if measuring || visibility <= f32::EPSILON { ui.set_invisible(); }
                egui::Frame::new().fill(palette().surface).stroke(egui::Stroke::new(1.0, palette().border))
                    .corner_radius(12).inner_margin(egui::Margin::symmetric(22, 18)).show(ui, |ui| {
                ui.set_width(520.0_f32.min((crate::ui::overlay_rect(ctx).width() - 48.0).max(240.0)));
                ui.horizontal(|ui| {
                    ui.heading(language.text("反馈问题", "Report an issue"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close |= icon_button(ui, Icon::X).on_hover_text(language.text("关闭", "Close")).clicked();
                    });
                });
                ui.add_space(12.0);
                let submitted = matches!(self.status, Status::Submitted(_));
                if let Some(draft) = &mut self.draft {
                    if let Status::Submitted(issue) = &self.status {
                        ui.label(language.text("反馈已提交，感谢你的帮助。", "Feedback submitted. Thank you."));
                        ui.hyperlink_to(format!("{} #{}", language.text("查看反馈", "View issue"), issue.number), &issue.url);
                    } else {
                        egui::ScrollArea::vertical().id_salt("feedback-body").auto_shrink([false, true]).max_height((crate::ui::overlay_rect(ctx).height() - 220.0).max(120.0)).show(ui, |ui| {
                            let scroll = &ui.spacing().scroll;
                            let gutter = if scroll.floating {
                                (scroll.bar_width + scroll.bar_inner_margin + scroll.bar_outer_margin + 4.0).max(14.0)
                            } else { 6.0 };
                            ui.set_width((ui.available_width() - gutter).max(1.0));
                            ui.add_enabled_ui(!self.locked, |ui| {
                                ui.horizontal(|ui| {
                                    for (kind, label) in [(Kind::Bug, language.text("缺陷", "Bug")), (Kind::Feature, language.text("功能", "Feature"))] {
                                        if crate::settings::choice_button(ui, label, draft.payload.kind == kind).clicked() {
                                            draft.payload.kind = kind;
                                        }
                                    }
                                });
                                ui.add_space(10.0);
                                let hint = if draft.payload.kind == Kind::Bug {
                                    language.text("发生了什么？如何重现？你期望出现什么结果？", "What happened? How can we reproduce it? What did you expect?")
                                } else { language.text("希望增加什么功能？它能解决什么问题？", "What feature would help, and what problem would it solve?") };
                                let hint = format!("{hint}\n{}", language.text("至少 10 个非空白字符", "At least 10 non-whitespace characters"));
                                let editor = description_editor(ui, &mut draft.payload.description, &hint);
                                if self.focus_input && !measuring && !preview_active && self.visible && visibility > f32::EPSILON { editor.request_focus(); self.focus_input = false; }
                            });
                            if self.validation_requested && !valid_description(&draft.payload.description) {
                                ui.colored_label(palette().error, language.text("请输入 10–5000 个非空白字符。", "Enter 10–5000 non-whitespace characters."));
                            }
                            ui.add_space(8.0);
                            let mut remove = None;
                            ui.horizontal_wrapped(|ui| {
                                for (index, texture) in self.thumbnails.iter().enumerate() {
                                    let (view, delete) = ui.push_id(index, |ui| {
                                        screenshot_thumbnail(ui, texture, !self.locked, language)
                                    }).inner;
                                    if view.clicked() && !delete.clicked() { preview_index = Some(index); }
                                    if delete.clicked() { remove = Some(index); }
                                }
                                if draft.payload.screenshots.len() < screenshots::MAX_IMAGES {
                                    let pending = self.image_task.is_pending();
                                    if ui.add_enabled_ui(!self.locked && !pending, |ui| screenshot_add_button(ui, pending, language.text("添加截图", "Add screenshots"))).inner.clicked() {
                                        self.image_error = None;
                                        self.image_task.begin(screenshots::Source::Pick);
                                    }
                                }
                            });
                            if let Some(index) = remove {
                                let _ = self.thumbnails.remove(index);
                                draft.payload.screenshots.remove(index);
                            }
                            if let Some(error) = &self.image_error {
                                ui.colored_label(palette().error, if error == "limit" {
                                    language.text("最多添加三张截图，请先移除已有截图。", "Attach up to three screenshots. Remove an image first.")
                                } else { language.text("无法读取截图，请选择有效的图片文件。", "Could not read this screenshot. Choose a valid image file.") });
                            }
                            ui.add_space(8.0);
                            egui::CollapsingHeader::new(language.text("附带信息", "Included information")).show(ui, |ui| show_context(ui, &draft.payload.context, language));
                        });
                    }
                }
                ui.add_space(12.0);
                match &self.status {
                    Status::Pending => { ui.colored_label(palette().error, language.text("暂时无法确认提交结果，内容已保留，请稍后再次提交。", "The result is not yet confirmed. Your report is preserved; submit again later.")); }
                    Status::Failed(failure) => { ui.colored_label(palette().error, failure_text(*failure, language)); }
                    _ => {}
                }
                let wait = self.retry_at.and_then(|at| at.checked_duration_since(Instant::now()));
                if let Some(wait) = wait {
                    ui.label(format!("{} {} {}", language.text("请等待", "Retry in"), wait.as_secs() + 1, language.text("秒后重试", "seconds")));
                    crate::ui::request_repaint_in(ctx, Duration::from_secs(1).min(wait));
                }
                ui.add_space(12.0);
                // Do not let the footer retain the Area's previous, taller form height.
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 32.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !submitted {
                        let valid = self.draft.as_ref().is_some_and(|d| valid_description(&d.payload.description));
                        let busy = self.task.is_pending();
                        send = ui.add_enabled_ui(valid && !busy && !self.image_task.is_pending() && wait.is_none(), |ui| submit_button(ui, language.text("提交", "Submit"), busy)).inner.clicked();
                        if self.draft.is_none() && !self.task.is_pending() { prepare = dialog_action_button(ui, language.text("重试", "Retry"), true).clicked(); }
                    }
                    close |= dialog_action_button(ui, if submitted { language.text("完成", "Done") } else { language.text("关闭", "Close") }, submitted).clicked();
                });
                });
            });
        if measuring && !preparing {
            let size = response.response.rect.size();
            self.layout_ready = self.opening_size.is_some_and(|previous| {
                (previous - size).abs().max_elem() < 0.5
            });
            self.opening_size = Some(size);
            // Only layout convergence requests another frame. Waiting for context
            // or restored thumbnails is woken by the worker completion event.
            ctx.request_repaint();
        }
        if self.visible && !preview_active && (close || response.should_close()
            || !measuring && enter && matches!(self.status, Status::Submitted(_))
        ) {
            self.visible = false;
            self.clear_preview();
            ctx.request_repaint();
        }
        if self.visible && !measuring && (send || enter && !matches!(self.status, Status::Submitted(_))) {
            self.send();
            ctx.request_repaint();
        }
        if prepare && let Some(seed) = self.seed.clone() {
            self.open(seed);
        }
        if let Some(index) = preview_index {
            self.open_preview(index);
            ctx.request_repaint();
        }
    }
}

// Consume plain Enter before TextEdit sees it; Shift+Enter stays a newline.
// IME candidate confirmation, commit frames and held keys must never submit.
fn submit_key(events: &mut Vec<egui::Event>, composing: &mut bool) -> bool {
    let mut blocked = *composing;
    for event in events.iter() {
        match event {
            egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                blocked = true;
                *composing = !text.is_empty();
            }
            egui::Event::Ime(egui::ImeEvent::Commit(_)) => {
                blocked = true;
                *composing = false;
            }
            _ => {}
        }
    }
    let mut submit = false;
    events.retain(|event| {
        if let egui::Event::Key {
            key: egui::Key::Enter,
            pressed: true,
            repeat,
            modifiers,
            ..
        } = event
            && modifiers.is_none()
            && !blocked
        {
            submit |= !repeat;
            return false;
        }
        true
    });
    submit
}

const SCREENSHOT_TILE_SIZE: f32 = 78.0;

fn screenshot_tile_color(ui: &egui::Ui, hovered: bool) -> egui::Color32 {
    egui::Color32::from_gray(match (ui.visuals().dark_mode, hovered && ui.is_enabled()) {
        (true, true) => 100,
        (true, false) => 82,
        (false, true) => 192,
        (false, false) => 208,
    })
}

fn screenshot_thumbnail(ui: &mut egui::Ui, texture: &egui::TextureHandle, removable: bool, language: AppLanguage) -> (egui::Response, egui::Response) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(SCREENSHOT_TILE_SIZE), egui::Sense::hover());
    let preview = ui.interact(rect, ui.make_persistent_id("preview-screenshot"), egui::Sense::click());
    let close_rect = egui::Rect::from_min_size(rect.right_top() + egui::vec2(-24.0, 2.0), egui::Vec2::splat(22.0));
    let response = ui.add_enabled_ui(removable, |ui| ui.interact(close_rect, ui.make_persistent_id("remove-screenshot"), egui::Sense::click())).inner;
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 6.0, screenshot_tile_color(ui, false));
        let available = rect.shrink(4.0);
        let image_size = texture.size_vec2();
        let scale = (available.width() / image_size.x).min(available.height() / image_size.y);
        egui::Image::from_texture(texture).paint_at(ui, egui::Rect::from_center_size(rect.center(), image_size * scale));
        ui.painter().circle_filled(close_rect.center(), 10.0, egui::Color32::from_gray(if response.hovered() && ui.is_enabled() { 70 } else { 100 }));
        crate::ui::paint_icon(ui, egui::Rect::from_center_size(close_rect.center(), egui::Vec2::splat(14.0)), Icon::X, egui::Color32::WHITE);
    }
    let label = language.text("移除截图", "Remove screenshot");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled() && removable, label));
    (preview.on_hover_text(language.text("预览截图", "Preview screenshot")).on_hover_cursor(egui::CursorIcon::PointingHand),
     response.on_hover_text(label).on_hover_cursor(egui::CursorIcon::PointingHand))
}

fn screenshot_add_button(ui: &mut egui::Ui, busy: bool, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(SCREENSHOT_TILE_SIZE), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 6.0, screenshot_tile_color(ui, response.hovered()));
        if busy {
            crate::ui::LoadingSpinner::new().color(egui::Color32::WHITE)
                .paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(24.0, 24.0)));
        } else {
            let center = rect.center();
            let stroke = egui::Stroke::new(3.0, egui::Color32::WHITE);
            ui.painter().line_segment([center - egui::vec2(12.0, 0.0), center + egui::vec2(12.0, 0.0)], stroke);
            ui.painter().line_segment([center - egui::vec2(0.0, 12.0), center + egui::vec2(0.0, 12.0)], stroke);
        }
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    response.on_hover_text(label).on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn submit_button(ui: &mut egui::Ui, label: &str, busy: bool) -> egui::Response {
    let text = if busy {
        format!("    {label}")
    } else {
        label.to_owned()
    };
    let response = dialog_action_button(ui, &text, true);
    if busy {
        let rect = egui::Rect::from_center_size(
            egui::pos2(response.rect.left() + 15.0, response.rect.center().y),
            egui::vec2(14.0, 14.0),
        );
        crate::ui::LoadingSpinner::new().color(egui::Color32::WHITE).paint_at(ui, rect);
    }
    response
}

fn failure_text(failure: Failure, l: AppLanguage) -> &'static str {
    match failure {
        Failure::Network => l.text("无法连接反馈服务，内容已保留，请检查网络后重试。", "Cannot reach the feedback service. Your report is preserved; check your connection and retry."),
        Failure::RateLimited => l.text("提交过于频繁，请稍后重试。", "Too many submissions. Please retry later."),
        Failure::Invalid => l.text("反馈内容不符合要求，请调整后重新提交。", "Please correct your report before submitting again."),
        Failure::Conflict => l.text("提交编号与已保存的反馈不一致，请保留当前内容并联系维护者。", "This submission ID conflicts with an existing report. Keep this draft and contact the maintainer."),
        Failure::Protocol => l.text("暂时无法确认提交结果，内容已保留，请稍后核对。", "The result could not be verified. Your report is preserved; check again later."),
        Failure::Storage => l.text("无法保存反馈草稿，请检查本地存储后重试。", "Could not save or restore the draft. Check local storage and retry."),
        Failure::Configuration => l.text("反馈服务地址配置无效。", "The feedback service address is invalid."),
        Failure::Service => l.text("反馈服务暂时不可用，内容已保留，请稍后重试。", "The feedback service is unavailable. Your report is preserved; retry later."),
    }
}

fn description_editor(ui: &mut egui::Ui, description: &mut String, hint: &str) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let counter_font = egui::TextStyle::Small.resolve(ui.style());
    // Reserve a separate footer inside the field, including when its text grows.
    let footer = ui.fonts_mut(|fonts| fonts.row_height(&counter_font)) + 12.0;
    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, width: f32| {
        let line_spacing = ui.spacing().extra_text_line_spacing;
        ui.fonts_mut(|fonts| {
            let mut job = egui::text::LayoutJob::simple(
                text.as_str().to_owned(),
                font.clone(),
                palette().text,
                width,
            );
            for section in &mut job.sections {
                section.format.line_height = Some(fonts.row_height(&font) + line_spacing);
            }
            fonts.layout_job(job)
        })
    };
    let response = ui.add(
        egui::TextEdit::multiline(description)
            .return_key(egui::KeyboardShortcut::new(
                egui::Modifiers::SHIFT,
                egui::Key::Enter,
            ))
            .font(egui::TextStyle::Body)
            .desired_rows(6)
            .desired_width(f32::INFINITY)
            .margin(egui::Margin { left: 10, right: 10, top: 8, bottom: footer.ceil().clamp(24.0, 127.0) as i8 })
            .layouter(&mut layouter)
            .char_limit(10_000)
            .hint_text(hint),
    );
    if ui.is_rect_visible(response.rect) {
        let count = character_count(description);
        ui.painter().text(
            response.rect.right_bottom() - egui::vec2(10.0, 7.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("{count}/5000"),
            counter_font,
            if count > 5000 { palette().error } else { palette().muted },
        );
    }
    response
}

fn show_context(ui: &mut egui::Ui, c: &model::Context, l: AppLanguage) {
    if let Some(b) = &c.book {
        context_group(
            ui,
            "feedback-book",
            l.text("当前书籍", "Current book"),
            |ui, width| {
                context_row(ui, width, l.text("原书书名", "Original title"), &b.title);
                if !b.authors.is_empty() {
                    context_row(ui, width, l.text("作者", "Authors"), &b.authors.join(", "));
                }
                context_row(
                    ui,
                    width,
                    l.text("格式", "Format"),
                    &format!(
                        "{} · {}",
                        b.format,
                        if b.layout == "fixed" {
                            l.text("固定版式", "Fixed layout")
                        } else {
                            l.text("流式版式", "Reflowable layout")
                        }
                    ),
                );
                if !b.languages.is_empty() {
                    context_row(
                        ui,
                        width,
                        l.text("原书语言", "Book language"),
                        &b.languages.join(", "),
                    );
                }
            },
        );
    }
    if c.reader.is_some() || c.location.is_some() {
        context_group(
            ui,
            "feedback-reading",
            l.text("阅读状态", "Reading state"),
            |ui, width| {
                if let Some(p) = &c.location {
                    if let Some(chapter) = &p.chapter {
                        context_row(ui, width, l.text("当前章节", "Chapter"), chapter);
                    }
                    if let Some(page) = p.page {
                        context_row(ui, width, l.text("PDF 页码", "PDF page"), &page.to_string());
                    }
                }
                if let Some(r) = &c.reader {
                    context_row(
                        ui,
                        width,
                        l.text("阅读模式", "Mode"),
                        if r.mode == "focus" {
                            l.text("专注模式", "Focus mode")
                        } else {
                            l.text("经典模式", "Classic mode")
                        },
                    );
                    context_row(
                        ui,
                        width,
                        l.text("排版", "Typesetting"),
                        if r.typesetting == "unified" {
                            l.text("统一版式", "Unified typesetting")
                        } else {
                            l.text("原书版式", "Original typesetting")
                        },
                    );
                    if let Some(pdf) = &r.pdf_reflow {
                        context_row(
                            ui,
                            width,
                            l.text("PDF 视图", "PDF view"),
                            match pdf.as_str() {
                                "local" => l.text("本地重排", "Local reflow"),
                                "ai" => l.text("AI 重排", "AI reflow"),
                                _ => l.text("原版", "Original"),
                            },
                        );
                    }
                    context_row(
                        ui,
                        width,
                        l.text("翻译", "Translation"),
                        &if r.translation.enabled {
                            format!(
                                "{} · {}",
                                l.text("已开启", "On"),
                                r.translation.target_language
                            )
                        } else {
                            l.text("已关闭", "Off").into()
                        },
                    );
                }
                if let Some(p) = &c.location {
                    if let Some(document) = &p.document {
                        context_row(ui, width, l.text("书内文档", "Document"), document);
                    }
                    if let Some(anchor) = &p.anchor {
                        context_row(ui, width, l.text("章节锚点", "Chapter anchor"), anchor);
                    }
                    if let Some(focus) = &p.focus {
                        context_row(
                            ui,
                            width,
                            l.text("当前聚焦块", "Focused block"),
                            &focus.range.start.node,
                        );
                        context_row(
                            ui,
                            width,
                            l.text("聚焦范围", "Focus source range"),
                            &focus.range.label(),
                        );
                        let count = format!(
                            "{}/{}{}",
                            focus.ranges.len(),
                            focus.total_ranges,
                            if focus.ranges.len() < focus.total_ranges {
                                l.text("（已截断）", " (truncated)")
                            } else {
                                ""
                            }
                        );
                        egui::CollapsingHeader::new(format!(
                            "{} · {count}",
                            l.text("成员范围", "Member ranges")
                        ))
                        .show(ui, |ui| {
                            for (index, range) in focus.ranges.iter().enumerate() {
                                context_row(ui, width, &(index + 1).to_string(), &range.label());
                            }
                        });
                    } else if c
                        .reader
                        .as_ref()
                        .is_some_and(|reader| reader.mode == "focus")
                    {
                        context_row(
                            ui,
                            width,
                            l.text("当前聚焦块", "Focused block"),
                            l.text("聚焦位置暂不可用", "Focus location unavailable"),
                        );
                    }
                }
            },
        );
    }
    context_group(
        ui,
        "feedback-environment",
        l.text("运行环境", "Environment"),
        |ui, width| {
            context_row(
                ui,
                width,
                l.text("应用版本", "App version"),
                &format!("Torto {}", c.app.version),
            );
            context_row(
                ui,
                width,
                l.text("操作系统", "OS"),
                &format!("{} {}", c.os.name, c.os.version),
            );
            context_row(ui, width, l.text("系统架构", "Architecture"), &c.os.arch);
            context_row(
                ui,
                width,
                l.text("窗口尺寸", "Window size"),
                &format!("{} × {} px", c.display.width, c.display.height),
            );
            context_row(
                ui,
                width,
                l.text("显示缩放", "Display scale"),
                &format!("{:.0}%", c.display.scale * 100.0),
            );
        },
    );
}

fn context_value_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - 100.0).max(80.0)
}

fn context_group(ui: &mut egui::Ui, id: &str, title: &str, rows: impl FnOnce(&mut egui::Ui, f32)) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title).strong());
    ui.add_space(4.0);
    let width = context_value_width(ui);
    ui.push_id(id, |ui| rows(ui, width));
    ui.add_space(10.0);
}

fn context_row(ui: &mut egui::Ui, width: f32, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.allocate_ui_with_layout(
            egui::vec2(88.0, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_width(88.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(label).color(palette().muted)).truncate(),
                );
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add(egui::Label::new(value).truncate().selectable(true));
            },
        );
    });
    ui.add_space(2.0);
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn remaining_retry(draft: &Draft) -> Option<Duration> {
    draft
        .retry_until
        .and_then(|at| at.checked_sub(now_seconds()))
        .filter(|s| *s > 0)
        .map(|s| Duration::from_secs(s.min(604_800)))
}

fn draft_path() -> Result<PathBuf, String> {
    crate::smoke::project_dirs()
        .map(|p| p.data_local_dir().join("feedback/draft.json"))
        .ok_or_else(|| "storage".into())
}

fn prepare(mut seed: OpenContext, restore: bool) -> Result<Output, String> {
    if restore {
        let path = draft_path()?;
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() <= 9 * 1024 * 1024 => {
                let bytes = std::fs::read(path).map_err(|_| "storage")?;
                let draft: Draft = serde_json::from_slice(&bytes).map_err(|_| "storage")?;
                transport::endpoint(&draft.endpoint)?;
                if draft.version != 1
                    || uuid::Uuid::parse_str(&draft.payload.request_id).is_err()
                    || !valid_description(&draft.payload.description)
                    || draft.payload.screenshots.len() > screenshots::MAX_IMAGES
                {
                    return Err("storage".into());
                }
                for screenshot in &draft.payload.screenshots {
                    screenshots::restore(screenshot.clone())?;
                }
                return Ok(Output::Ready(draft, true));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("storage".into()),
        }
    }
    seed.context.os = collect_os();
    if let Some(id) = seed.pdf_id
        && let Some(reader) = &mut seed.context.reader
    {
        reader.pdf_reflow = Some(
            match crate::plugins::pdf_native::selected(&id) {
                crate::plugins::pdf_native::TextSource::Native => "local",
                crate::plugins::pdf_native::TextSource::Ocr => "ai",
            }
            .into(),
        );
    }
    let endpoint = std::env::var("TORTO_FEEDBACK_URL")
        .ok()
        .or_else(|| option_env!("TORTO_FEEDBACK_URL").map(str::to_owned))
        .unwrap_or_else(|| transport::DEFAULT_ENDPOINT.into());
    transport::endpoint(&endpoint)?;
    Ok(Output::Ready(
        Draft {
            version: 1,
            endpoint,
            retry_until: None,
            payload: Payload {
                request_id: uuid::Uuid::new_v4().to_string(),
                kind: Kind::Bug,
                description: String::new(),
                screenshots: Vec::new(),
                context: seed.context,
            },
        },
        false,
    ))
}

fn collect_os() -> model::Os {
    #[cfg(target_os = "windows")]
    let version = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd.exe")
            .args(["/c", "ver"])
            .creation_flags(0x0800_0000)
            .output()
            .ok()
            .and_then(|o| {
                regex::Regex::new(r"\d+\.\d+\.\d+(?:\.\d+)?")
                    .ok()?
                    .find(&String::from_utf8_lossy(&o.stdout))
                    .map(|m| m.as_str().to_owned())
            })
    };
    #[cfg(target_os = "macos")]
    let version = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let version = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("VERSION_ID=")
                    .map(|v| v.trim_matches('"').to_owned())
            })
        });
    model::Os {
        name: match std::env::consts::OS {
            "windows" => "Windows",
            "macos" => "macOS",
            other => other,
        }
        .into(),
        version: clipped(&version.unwrap_or_else(|| "unknown".into()), 100),
        arch: std::env::consts::ARCH.into(),
    }
}

async fn execute(work: Work) -> Result<Output, String> {
    match work {
        Work::Prepare(seed, restore) => tokio::task::spawn_blocking(move || prepare(seed, restore))
            .await
            .map_err(|_| "storage")?,
        Work::Send(mut draft) => {
            let path = tokio::task::spawn_blocking(draft_path)
                .await
                .map_err(|_| "storage")??;
            let saved = draft.clone();
            let saved_path = path.clone();
            tokio::task::spawn_blocking(move || save_draft(&saved_path, &saved))
                .await
                .map_err(|_| "storage")?
                .map_err(|_| "storage")?;
            let result = transport::submit(&draft).await;
            let retry = match &result {
                Outcome::Pending(wait) => Some(*wait),
                Outcome::Failed(_, wait) => *wait,
                _ => None,
            };
            draft.retry_until = retry.map(|wait| now_seconds() + wait.as_secs());
            let saved = draft.clone();
            let submitted = matches!(
                result,
                Outcome::Submitted(_) | Outcome::Failed(Failure::Invalid, _)
            );
            let _ = tokio::task::spawn_blocking(move || {
                if submitted {
                    let _ = std::fs::remove_file(path);
                } else {
                    let _ = save_draft(&path, &saved);
                }
            })
            .await;
            Ok(Output::Sent(draft, result))
        }
    }
}

fn save_draft(path: &std::path::Path, draft: &Draft) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("draft directory"))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    crate::persistence::write_json_atomic(path, draft)
}
