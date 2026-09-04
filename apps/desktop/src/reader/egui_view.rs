use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange};
use egui::{Color32, Pos2, Rect, RichText, TextureId, Vec2};
use rebook_layout::linebreak::{MeasuredCluster, SpacingAdjustment, plan_measured_text};
use rebook_layout::{SpreadMode, reading_content_left, reading_content_width};
use rebook_reader::{PageDirection, ReaderImage, SelectionGranularity};
use unicode_segmentation::UnicodeSegmentation;

use super::chat_autocomplete::{
    ChatReference, ChatReferenceKind, chat_reference_token, move_suggestion_index,
};
use super::chat_markdown::ChatMarkdownState;
use super::{
    AnnotationDraft, AssistantPanel, DesktopReader, GeneratedTocDraft, ImagePointerState,
    ImagePressCandidate, MOTION_EPSILON, ReaderOverlay, SelectedImage, SidebarTab,
    focus_scroll_content_height, focus_unit_screen_center_y, focus_unit_target_offset_for_rect,
};
use crate::plugins::{
    BookSearchResult, ChatCommand, ChatRole, PdfOcrViewMode, chat_command_suggestions,
};
use crate::preferences::{AppLanguage, AppTheme};
use crate::settings::ReaderSettingsChange;
use crate::ui::{
    Icon, ToastKind, decode_color_image, dialog_action_button, footnote_link_color, icon,
    icon_button, navigation_button, navigation_text_button, paint_icon, palette,
    selectable_icon_button, show_toast, small_icon_button,
};

pub(super) const SIDEBAR_WIDTH: f32 = 256.0;
const SIDEBAR_MIN_WIDTH: f32 = 220.0;
const SIDEBAR_MAX_WIDTH: f32 = 420.0;
const SIDEBAR_PADDING: i8 = 8;
pub(super) const ASSISTANT_WIDTH: f32 = 340.0;
const ASSISTANT_MIN_WIDTH: f32 = 300.0;
const ASSISTANT_MAX_WIDTH: f32 = 560.0;
const ASSISTANT_SIDE_PADDING: i8 = 14;
const ASSISTANT_SCROLLBAR_GUTTER: f32 = 14.0;
const MIN_READER_CONTENT_WIDTH: f32 = 200.0;
const PANEL_RESIZE_HANDLE_WIDTH: f32 = 8.0;
const ASSISTANT_EMPTY_TOP_PADDING: f32 = 12.0;
const ASSISTANT_BOTTOM_PADDING: f32 = 12.0;
const ASSISTANT_COMPOSER_RESERVED_HEIGHT: f32 = 52.0;
const ASSISTANT_INPUT_HEIGHT: f32 = 32.0;
const ASSISTANT_SELECTION_SCROLL_EDGE: f32 = 36.0;
const ASSISTANT_SELECTION_SCROLL_MIN_SPEED: f32 = 90.0;
const ASSISTANT_SELECTION_SCROLL_MAX_SPEED: f32 = 640.0;
const ASSISTANT_KEYBOARD_SCROLL_STEP: f32 = 64.0;
const TOOLBAR_HEIGHT: f32 = 48.0;
const TOOLBAR_CONTROL_SIZE: f32 = 32.0;
const TOOLBAR_TITLE_SIZE: f32 = 15.0;
const TOC_ROW_HEIGHT: f32 = 36.0;
const TOC_SCROLL_ID_SALT: &str = "reader-toc-scroll";
const WHEEL_PAGE_THRESHOLD: f32 = 18.0;
const WHEEL_TURN_COOLDOWN: Duration = Duration::from_millis(120);
const IMAGE_PREVIEW_MARGIN: f32 = 48.0;
const IMAGE_PREVIEW_MIN_ZOOM: f32 = 0.25;
const IMAGE_PREVIEW_MAX_ZOOM: f32 = 8.0;
const IMAGE_PREVIEW_WHEEL_SPEED: f32 = 0.0025;
const IMAGE_LONG_PRESS_DURATION: Duration = Duration::from_millis(500);
const IMAGE_LONG_PRESS_MAX_TRAVEL: f32 = 8.0;

struct FootnoteTextLayout {
    lines: Vec<FootnoteTextLine>,
    height: f32,
}

struct FootnoteTextLine {
    runs: Vec<FootnoteTextRun>,
    height: f32,
}

struct FootnoteTextRun {
    x: f32,
    galley: Arc<egui::Galley>,
}

fn footnote_cluster_spacing(
    adjustments: &[SpacingAdjustment],
    range: &std::ops::Range<usize>,
) -> f32 {
    adjustments
        .iter()
        .find(|adjustment| {
            adjustment.range.start <= range.start && adjustment.range.end >= range.end
        })
        .map_or(0.0, |adjustment| adjustment.amount)
}

fn optimized_footnote_text_layout(
    ctx: &egui::Context,
    text: &str,
    font: &egui::FontId,
    color: Color32,
    width: f32,
) -> FootnoteTextLayout {
    ctx.fonts_mut(|fonts| {
        let graphemes = text
            .grapheme_indices(true)
            .map(|(start, grapheme)| {
                let range = start..start + grapheme.len();
                let advance = fonts
                    .layout_no_wrap(grapheme.to_owned(), font.clone(), color)
                    .size()
                    .x;
                MeasuredCluster {
                    range,
                    advance,
                    em: font.size,
                    ordinary_baseline: true,
                    footnote_reference: false,
                }
            })
            .collect::<Vec<_>>();
        let Some(plan) = plan_measured_text(text, &graphemes, width, 0.0, font.size) else {
            let galley = fonts.layout(text.to_owned(), font.clone(), color, width);
            return FootnoteTextLayout {
                height: galley.size().y,
                lines: vec![FootnoteTextLine {
                    height: galley.size().y,
                    runs: vec![FootnoteTextRun { x: 0.0, galley }],
                }],
            };
        };

        let mut lines = Vec::with_capacity(plan.lines.len());
        let mut cluster_start = 0;
        for (line_index, line) in plan.lines.iter().enumerate() {
            let cluster_end = cluster_start
                + usize::try_from(line.cluster_count).unwrap_or(graphemes.len() - cluster_start);
            let cluster_end = cluster_end.min(graphemes.len());
            let mut visible_end = cluster_end;
            if line_index + 1 < plan.lines.len() {
                while visible_end > cluster_start
                    && text[graphemes[visible_end - 1].range.clone()]
                        .chars()
                        .all(|character| character.is_whitespace() && character != '\u{00a0}')
                {
                    visible_end -= 1;
                }
            }
            let mut x = 0.0;
            let mut height = 0.0_f32;
            let mut runs = Vec::with_capacity(visible_end.saturating_sub(cluster_start));
            for cluster in &graphemes[cluster_start..visible_end] {
                let galley = fonts.layout_no_wrap(
                    text[cluster.range.clone()].to_owned(),
                    font.clone(),
                    color,
                );
                height = height.max(galley.size().y);
                runs.push(FootnoteTextRun { x, galley });
                x += cluster.advance + footnote_cluster_spacing(&plan.adjustments, &cluster.range);
            }
            if !runs.is_empty() {
                lines.push(FootnoteTextLine {
                    runs,
                    height: height.max(font.size),
                });
            }
            cluster_start = cluster_end;
        }
        if lines.is_empty() {
            let galley = fonts.layout(text.to_owned(), font.clone(), color, width);
            return FootnoteTextLayout {
                height: galley.size().y,
                lines: vec![FootnoteTextLine {
                    height: galley.size().y,
                    runs: vec![FootnoteTextRun { x: 0.0, galley }],
                }],
            };
        }
        FootnoteTextLayout {
            height: lines.iter().map(|line| line.height).sum(),
            lines,
        }
    })
}

fn paint_footnote_text_line(ui: &mut egui::Ui, line: &FootnoteTextLine, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(1.0), line.height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    for run in &line.runs {
        painter.galley(
            Pos2::new(rect.left() + run.x, rect.top()),
            Arc::clone(&run.galley),
            color,
        );
    }
}

const fn should_hide_reader_cursor(
    is_focus_mode: bool,
    hide_cursor_in_focus_mode: bool,
    interaction_blocked: bool,
    floating_sidebar_visible: bool,
) -> bool {
    is_focus_mode && hide_cursor_in_focus_mode && !interaction_blocked && !floating_sidebar_visible
}

const fn reader_menu_close_requested(overlay: ReaderOverlay, escape_pressed: bool) -> bool {
    matches!(overlay, ReaderOverlay::Menu) && escape_pressed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClassicNavigationAction {
    PreviousReadingUnit,
    NextReadingUnit,
    PreviousPage,
    NextPage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TocKeyboardAction {
    Previous,
    Next,
    Expand,
    Collapse,
    Activate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ModifierTapState {
    #[default]
    Idle,
    Armed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModifierTapInput {
    CleanPress,
    ChordedPress,
    Release,
    OtherInput,
    FocusLost,
}

fn advance_modifier_tap(state: &mut ModifierTapState, input: ModifierTapInput) -> bool {
    match input {
        ModifierTapInput::CleanPress => *state = ModifierTapState::Armed,
        ModifierTapInput::ChordedPress => *state = ModifierTapState::Cancelled,
        ModifierTapInput::OtherInput if *state == ModifierTapState::Armed => {
            *state = ModifierTapState::Cancelled;
        }
        ModifierTapInput::Release => {
            let triggered = *state == ModifierTapState::Armed;
            *state = ModifierTapState::Idle;
            return triggered;
        }
        ModifierTapInput::FocusLost => *state = ModifierTapState::Idle,
        ModifierTapInput::OtherInput => {}
    }
    false
}

fn modifier_tap_triggered(
    state: &mut ModifierTapState,
    events: &[egui::Event],
    modifier_key: egui::Key,
) -> bool {
    let mut triggered = false;
    for event in events {
        let input = match event {
            egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } if *key == modifier_key => {
                if *repeat {
                    continue;
                }
                if modifiers.ctrl || modifiers.shift || modifiers.mac_cmd || modifiers.command {
                    ModifierTapInput::ChordedPress
                } else {
                    ModifierTapInput::CleanPress
                }
            }
            egui::Event::Key {
                key,
                pressed: false,
                ..
            } if *key == modifier_key => ModifierTapInput::Release,
            egui::Event::Key { pressed: true, .. }
            | egui::Event::Text(_)
            | egui::Event::Copy
            | egui::Event::Cut
            | egui::Event::Paste(_)
            | egui::Event::PointerButton { pressed: true, .. }
            | egui::Event::MouseWheel { .. }
            | egui::Event::Zoom(_)
            | egui::Event::Rotate(_) => ModifierTapInput::OtherInput,
            egui::Event::ModifiersChanged(modifiers)
                if modifiers.ctrl || modifiers.shift || modifiers.mac_cmd || modifiers.command =>
            {
                ModifierTapInput::OtherInput
            }
            egui::Event::WindowFocused(false) => ModifierTapInput::FocusLost,
            _ => continue,
        };
        triggered |= advance_modifier_tap(state, input);
    }
    triggered
}

fn is_bare_left_alt_shortcut(shortcut: egui::KeyboardShortcut) -> bool {
    shortcut.logical_key == egui::Key::AltLeft && shortcut.modifiers == egui::Modifiers::NONE
}

const fn toc_keyboard_navigation_enabled(is_focus_mode: bool, sidebar_pinned: bool) -> bool {
    is_focus_mode || !sidebar_pinned
}

fn shortcut_has_fresh_press(input: &egui::InputState, shortcut: &egui::KeyboardShortcut) -> bool {
    input.raw.events.iter().any(|event| {
        matches!(
            event,
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } if *key == shortcut.logical_key
                && modifiers.matches_logically(shortcut.modifiers)
        )
    })
}

fn next_toc_keyboard_row(
    current: Option<usize>,
    active: Option<usize>,
    row_count: usize,
    direction: PageDirection,
) -> Option<usize> {
    if row_count == 0 {
        return None;
    }
    let base = match current {
        Some(row) if row < row_count => Some(row),
        _ => match active {
            Some(row) if row < row_count => Some(row),
            _ => None,
        },
    };
    Some(match direction {
        PageDirection::Previous => base.unwrap_or(row_count).saturating_sub(1),
        PageDirection::Next => base.map_or(0, |row| row.saturating_add(1).min(row_count - 1)),
    })
}

const fn toc_expansion_target(
    action: TocKeyboardAction,
    has_children: bool,
    expanded: bool,
) -> Option<bool> {
    match (action, has_children, expanded) {
        (TocKeyboardAction::Expand, true, false) => Some(true),
        (TocKeyboardAction::Collapse, true, true) => Some(false),
        _ => None,
    }
}

const fn classic_navigation_action(
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    page_up: bool,
    page_down: bool,
    space: bool,
) -> Option<ClassicNavigationAction> {
    if left {
        Some(ClassicNavigationAction::PreviousReadingUnit)
    } else if right {
        Some(ClassicNavigationAction::NextReadingUnit)
    } else if up || page_up {
        Some(ClassicNavigationAction::PreviousPage)
    } else if down || page_down || space {
        Some(ClassicNavigationAction::NextPage)
    } else {
        None
    }
}

#[derive(Clone, Copy)]
struct AssistantComposerKeys {
    input_had_focus: bool,
    initial_suggestion_count: usize,
    movement: AssistantSuggestionMovement,
    acceptance: AssistantSuggestionAcceptance,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssistantSuggestionMovement {
    None,
    Forward,
    Backward,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssistantSuggestionAcceptance {
    None,
    Tab,
    Enter,
}

struct AssistantComposerRender {
    composer_rect: Rect,
    input_response: egui::Response,
    submit: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ReaderFramePlan {
    pub(crate) rect: Rect,
    pub(crate) scene_id: u64,
    pub(crate) scene_revision: u64,
    pub(crate) background: peniko::Color,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ReaderPageTexture {
    pub(crate) id: TextureId,
    pub(crate) size: Vec2,
}

fn selected_image_layer_id() -> egui::LayerId {
    // This is a reader-content decoration, not a global overlay. Keeping it in
    // the middle order lets foreground modals and their backdrops cover it.
    egui::LayerId::new(egui::Order::Middle, egui::Id::new("reader-selected-image"))
}

fn constrained_panel_widths(
    viewport_width: f32,
    sidebar_width: f32,
    assistant_width: f32,
    sidebar_consumes_width: bool,
    assistant_consumes_width: bool,
) -> (f32, f32) {
    let mut sidebar_width = sidebar_width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
    let mut assistant_width = assistant_width.clamp(ASSISTANT_MIN_WIDTH, ASSISTANT_MAX_WIDTH);
    let available = (viewport_width - MIN_READER_CONTENT_WIDTH).max(0.0);

    match (sidebar_consumes_width, assistant_consumes_width) {
        (true, true) => {
            let minimum_total = SIDEBAR_MIN_WIDTH + ASSISTANT_MIN_WIDTH;
            if available < minimum_total {
                sidebar_width = available * SIDEBAR_MIN_WIDTH / minimum_total;
                assistant_width = available - sidebar_width;
            } else {
                let mut excess = (sidebar_width + assistant_width - available).max(0.0);
                let assistant_reduction =
                    excess.min((assistant_width - ASSISTANT_MIN_WIDTH).max(0.0));
                assistant_width -= assistant_reduction;
                excess -= assistant_reduction;
                sidebar_width -= excess.min((sidebar_width - SIDEBAR_MIN_WIDTH).max(0.0));
            }
        }
        (true, false) => sidebar_width = sidebar_width.min(available),
        (false, true) => assistant_width = assistant_width.min(available),
        (false, false) => {}
    }

    (sidebar_width, assistant_width)
}

fn panel_resize_pointer(ctx: &egui::Context, id: &'static str, edge_x: f32) -> Option<f32> {
    let viewport = ctx.content_rect();
    let response = egui::Area::new(id.into())
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(
            edge_x - PANEL_RESIZE_HANDLE_WIDTH / 2.0,
            viewport.top(),
        ))
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(PANEL_RESIZE_HANDLE_WIDTH, viewport.height()),
                egui::Sense::drag(),
            );
            let response = response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            let stroke = if response.dragged() {
                egui::Stroke::new(2.0, palette().accent)
            } else if response.hovered() {
                egui::Stroke::new(1.0, palette().muted)
            } else {
                egui::Stroke::new(1.0, palette().border)
            };
            ui.painter()
                .vline(rect.center().x, rect.top()..=rect.bottom(), stroke);
            response
        })
        .inner;
    response
        .dragged()
        .then(|| response.interact_pointer_pos().map(|position| position.x))
        .flatten()
}

fn pdf_toc_editor_table(
    ui: &mut egui::Ui,
    draft: &mut GeneratedTocDraft,
    language: AppLanguage,
    page_count: usize,
    max_height: f32,
) -> Option<usize> {
    let column_spacing = ui.spacing().item_spacing.x;
    let level_width = 52.0;
    let page_width = 68.0;
    let action_width = 44.0;
    let title_width = (ui.available_width()
        - level_width
        - page_width
        - action_width
        - column_spacing * 3.0
        - 12.0)
        .clamp(140.0, 360.0);
    let header = |text| {
        egui::Label::new(
            RichText::new(text)
                .size(crate::ui::scaled_font_size(12.0))
                .strong()
                .color(palette().muted),
        )
    };
    ui.horizontal(|ui| {
        ui.add_sized([level_width, 24.0], header(language.text("层级", "Level")));
        ui.add_sized(
            [title_width, 24.0],
            header(language.text("目录标题", "Contents title")),
        );
        ui.add_sized([page_width, 24.0], header(language.text("页码", "Page")));
        ui.add_sized(
            [action_width, 24.0],
            header(language.text("操作", "Action")),
        );
    });
    ui.separator();
    ui.add_space(4.0);
    let mut remove = None;
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, entry) in draft.entries.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [level_width, 30.0],
                        egui::DragValue::new(&mut entry.depth)
                            .range(0..=6)
                            .prefix("L"),
                    )
                    .on_hover_text(language.text("目录层级", "Hierarchy level"));
                    ui.add_sized(
                        [title_width, 30.0],
                        egui::TextEdit::singleline(&mut entry.title)
                            .vertical_align(egui::Align::Center),
                    );
                    ui.add_sized(
                        [page_width, 30.0],
                        egui::DragValue::new(&mut entry.physical_page).range(1..=page_count),
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(action_width, 30.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            if small_icon_button(ui, Icon::Trash2)
                                .on_hover_text(language.text("删除", "Delete"))
                                .clicked()
                            {
                                remove = Some(index);
                            }
                        },
                    );
                });
            }
        });
    remove
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "reader page geometry is viewport-bounded and represented as f32 by egui"
)]
fn page_image_left(page: &rebook_renderer::PageDisplayList) -> Option<f32> {
    page.image_bounds()
        .map(|bounds| bounds.x0)
        .filter(|left| left.is_finite())
        .map(|left| left as f32)
}

fn uses_pdf_page_alignment(format: rebook_formats::BookFormat, ocr_mode: PdfOcrViewMode) -> bool {
    format == rebook_formats::BookFormat::Pdf && ocr_mode == PdfOcrViewMode::Original
}

fn supports_image_preview(format: rebook_formats::BookFormat, ocr_mode: PdfOcrViewMode) -> bool {
    !uses_pdf_page_alignment(format, ocr_mode)
}

impl DesktopReader {
    pub(crate) fn ui(
        &mut self,
        root_ui: &mut egui::Ui,
        page_texture: Option<ReaderPageTexture>,
        interaction_blocked: bool,
    ) -> ReaderFramePlan {
        let ctx = root_ui.ctx().clone();
        let now = Instant::now();
        self.advance_frame(now);
        self.apply_pending_focus_wheel_turn();
        self.copy_shortcut(&ctx, interaction_blocked);
        self.keyboard_shortcuts(&ctx, interaction_blocked);
        self.request_frame_repaint(&ctx);
        if let Some(deadline) = self.next_transient_message_deadline() {
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
        }

        let (sidebar_progress, assistant_progress) = self.show_side_panels(root_ui);

        let background = self.reader.style().background;
        let background_ui = color32(background);
        let mut page_rect = Rect::NOTHING;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(background_ui))
            .show(root_ui, |ui| {
                self.toolbar(ui, background_ui);
                let size = Vec2::new(ui.available_width(), (ui.available_height() - 3.0).max(1.0));
                if self.is_scroll_mode() {
                    page_rect = self.scroll_content(
                        ui,
                        size,
                        page_texture,
                        background_ui,
                        interaction_blocked,
                    );
                } else {
                    let response = if let Some(texture) = page_texture {
                        let (rect, response) =
                            ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                        let painter = ui.painter().with_clip_rect(rect);
                        painter.rect_filled(rect, 0.0, background_ui);
                        let texture_rect = page_texture_destination(rect, texture.size);
                        painter.image(
                            texture.id,
                            texture_rect,
                            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                        response
                    } else {
                        let (rect, response) =
                            ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                        ui.painter().rect_filled(rect, 0.0, background_ui);
                        response
                    };
                    page_rect = response.rect;
                    if self.image_preview.is_none() {
                        self.pointer_interaction(&response);
                        self.wheel_interaction(&response, interaction_blocked);
                    }
                }
                self.resize_canvas(f64::from(page_rect.width()), f64::from(page_rect.height()));

                let progress = unit_f32(self.progress());
                let (track, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 3.0),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .rect_filled(track, 0.0, Color32::from_black_alpha(18));
                let filled = Rect::from_min_size(
                    track.min,
                    Vec2::new(track.width() * progress, track.height()),
                );
                ui.painter().rect_filled(filled, 0.0, palette().accent);
            });

        let floating_sidebar_visible = !self.ui.sidebar_pinned && sidebar_progress > 0.001;
        if floating_sidebar_visible {
            self.floating_sidebar(&ctx, sidebar_progress);
        }
        if self.is_focus_mode() {
            self.focus_actions_overlay(&ctx, page_rect);
            self.focus_assistant_overlay(&ctx, page_rect);
        }
        self.focus_footnote_overlay(&ctx, page_rect);
        self.resize_side_panels(
            &ctx,
            sidebar_progress,
            assistant_progress,
            interaction_blocked,
        );
        self.menu(&ctx);
        self.selected_image_overlay(&ctx, page_rect);
        if !self.is_focus_mode() {
            self.selection_actions(&ctx, page_rect);
        }
        self.image_preview_overlay(&ctx);
        self.feedback(&ctx);
        self.pdf_toc_review(&ctx);

        if should_hide_reader_cursor(
            self.is_focus_mode(),
            super::resolved_focus_cursor_hidden(
                self.hide_cursor_in_focus_mode,
                self.focus_cursor_hidden_override,
            ),
            interaction_blocked,
            floating_sidebar_visible,
        ) {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }

        ReaderFramePlan {
            rect: page_rect,
            scene_id: self.scene_id,
            scene_revision: self.scene_revision,
            background: peniko::Color::from_rgba8(
                background.red,
                background.green,
                background.blue,
                background.alpha,
            ),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "scroll layout, viewport synchronization, and its controls share one egui frame"
    )]
    fn scroll_content(
        &mut self,
        ui: &mut egui::Ui,
        size: Vec2,
        page_texture: Option<ReaderPageTexture>,
        background: Color32,
        interaction_blocked: bool,
    ) -> Rect {
        let layout = match self.current_scroll_layout() {
            Ok(layout) => layout,
            Err(error) => {
                self.error = Some(format!("生成滑动章节失败：{error}"));
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                ui.painter().rect_filled(rect, 0.0, background);
                return rect;
            }
        };
        let rebuild_focus_units = self.is_focus_mode() && self.focus_units.is_empty();
        if rebuild_focus_units {
            self.rebuild_focus_units(&layout);
            if let Some(direction) = self.pending_reading_unit_entry
                && !self.focus_units.is_empty()
            {
                self.focus_unit_index = match direction {
                    PageDirection::Previous => self.focus_units.len() - 1,
                    PageDirection::Next => 0,
                };
                self.focus_anchor = self
                    .focus_units
                    .get(self.focus_unit_index)
                    .map(|unit| unit.range.start.clone());
                self.sync_focus_chat_session();
                self.sync_focus_selected_image();
            }
            // The page texture handed to this UI pass was rendered before the
            // newly selected focus unit existed. Schedule one more frame so the
            // rebuilt image underlay and complete paragraph overlay become the
            // texture that is actually presented, even when no animation follows.
            ui.ctx().request_repaint();
        }
        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt("reader-section-scroll")
            .max_height(size.y)
            .auto_shrink([false, false]);
        if self.is_focus_mode() {
            // Focus mode maps wheel and keyboard input to reading-unit navigation,
            // but the visible scrollbar must remain directly interactive.
            scroll_area = scroll_area.scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR);
        }
        if rebuild_focus_units || (self.is_focus_mode() && self.scroll_viewport.is_none()) {
            self.focus_target_offset = self.focus_unit_target_offset(size.y);
            self.ui.focus_scroll_motion = None;
        }
        if let Some(motion) = self.ui.focus_scroll_motion {
            scroll_area = scroll_area.vertical_scroll_offset(motion.value);
        } else if let Some(target) = self.focus_target_offset.take() {
            scroll_area = scroll_area.vertical_scroll_offset(target);
        }
        if self.is_focus_mode() {
            self.scroll_target_position = None;
            self.pending_reading_unit_entry = None;
        } else if let Some(direction) = self.pending_reading_unit_entry.take() {
            self.scroll_target_position = None;
            self.scroll_target_source = None;
            let offset = match direction {
                PageDirection::Previous => layout.content_height,
                PageDirection::Next => 0.0,
            };
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        } else if let Some(target) = self.scroll_target_source.take()
            && let Some(top) = layout.source_top(&target)
        {
            self.scroll_target_position = None;
            scroll_area = scroll_area.vertical_scroll_offset(top);
        } else if let Some(target) = self.scroll_target_position.take()
            && let Some(top) = layout.page_top(target)
        {
            let first = layout.pages.first().map(|entry| entry.position);
            let target_offset = if first == Some(target) { 0.0 } else { top };
            scroll_area = scroll_area.vertical_scroll_offset(target_offset);
        }

        let mut page_rect = Rect::NOTHING;
        let keyboard_scroll_delta = std::mem::take(&mut self.pending_keyboard_scroll_delta);
        let scroll_output = scroll_area.show_viewport(ui, |ui, viewport| {
            ui.set_width(viewport.width());
            let content_padding = self.scroll_content_padding(viewport.height());
            let scroll_content_height = if self.is_focus_mode() {
                focus_scroll_content_height(layout.content_height, viewport.height())
            } else {
                (layout.content_height + content_padding * 2.0).max(viewport.height())
            };
            ui.set_height(scroll_content_height);
            if keyboard_scroll_delta.abs() > f32::EPSILON {
                ui.scroll_with_delta(Vec2::new(0.0, keyboard_scroll_delta));
            }
            // ScrollArea rounds the moving content origin to physical pixels. Rebuilding
            // the viewport from that rounded origin plus the unrounded logical offset
            // leaks the rounding residue into the page texture position and produces a
            // one-pixel wobble near the end of programmatic scrolling. Its clip rect is
            // already the stable viewport in screen coordinates.
            let visible_rect = ui.clip_rect();
            page_rect = visible_rect;
            let painter = ui.painter().with_clip_rect(visible_rect);
            painter.rect_filled(visible_rect, 0.0, background);
            if let Some(texture) = page_texture {
                painter.image(
                    texture.id,
                    page_texture_destination(visible_rect, texture.size),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            let response = ui.interact(
                visible_rect,
                ui.id().with("scroll-reader-viewport"),
                egui::Sense::click_and_drag(),
            );
            self.update_scroll_viewport(
                ui.ctx(),
                super::ScrollViewportState {
                    offset_y: viewport.min.y,
                    size: viewport.size(),
                },
            );
            if self.image_preview.is_none() && !interaction_blocked {
                self.pointer_interaction(&response);
                if self.is_focus_mode() {
                    self.focus_wheel_interaction(&response);
                } else {
                    self.scroll_boundary_wheel_interaction(&response, interaction_blocked);
                }
            }
        });
        if self.is_focus_mode() {
            self.focus_scrollbar_navigation(ui.ctx(), &scroll_output, size.y);
        }
        page_rect
    }

    fn focus_scrollbar_navigation(
        &mut self,
        ctx: &egui::Context,
        output: &egui::scroll_area::ScrollAreaOutput<()>,
        viewport_height: f32,
    ) {
        let target_id = output.id.with("focus-scrollbar-target");
        let (primary_down, primary_clicked, primary_released) = ctx.input(|input| {
            (
                input.pointer.primary_down(),
                input.pointer.primary_clicked(),
                input.pointer.button_released(egui::PointerButton::Primary),
            )
        });
        let manipulating =
            output.state.vertical_scroll_bar_interacting() && (primary_down || primary_clicked);
        if manipulating
            && let Some(index) = focus_unit_index_for_scroll_offset(
                self.focus_units.iter().map(|unit| unit.rect),
                output.state.offset.y,
                viewport_height,
            )
            && let Some(target_offset) = self
                .focus_units
                .get(index)
                .map(|unit| focus_unit_target_offset_for_rect(unit.rect, viewport_height))
        {
            // The native scroll bar supplies a continuous offset. Quantize its
            // state to the nearest focus unit so dragging the handle previews
            // whole paragraphs instead of leaving the reader between them.
            let mut state = output.state;
            state.offset.y = target_offset;
            state.store(ctx, output.id);
            self.ui.focus_scroll_motion = None;
            self.focus_target_offset = None;
            let changed = self
                .scroll_viewport
                .is_none_or(|viewport| (viewport.offset_y - target_offset).abs() > 0.1);
            self.scroll_viewport = Some(super::ScrollViewportState {
                offset_y: target_offset,
                size: output.inner_rect.size(),
            });
            if changed {
                self.bump_scene_revision();
            }
            ctx.data_mut(|data| data.insert_temp(target_id, index));
            ctx.request_repaint();
        }

        if primary_released {
            if let Some(index) = ctx.data_mut(|data| data.remove_temp::<usize>(target_id)) {
                self.select_focus_unit(index);
            }
        } else if !primary_down && !primary_clicked {
            ctx.data_mut(|data| data.remove_temp::<usize>(target_id));
        }
    }

    fn show_side_panels(&mut self, root_ui: &mut egui::Ui) -> (f32, f32) {
        let sidebar_progress = self.ui.sidebar_motion.value.clamp(0.0, 1.0);
        let assistant_progress = self.ui.assistant_motion.value.clamp(0.0, 1.0);
        let viewport_width = root_ui.ctx().content_rect().width();
        let sidebar_consumes_width =
            !self.is_focus_mode() && self.ui.sidebar_pinned && sidebar_progress > 0.001;
        let assistant_consumes_width = !self.is_focus_mode()
            && self.ui.assistant_panel.is_some()
            && assistant_progress > 0.001;
        (self.ui.sidebar_width, self.ui.assistant_width) = constrained_panel_widths(
            viewport_width,
            self.ui.sidebar_width,
            self.ui.assistant_width,
            sidebar_consumes_width,
            assistant_consumes_width,
        );
        if sidebar_consumes_width {
            egui::Panel::left("reader-sidebar")
                .exact_size(self.ui.sidebar_width * sidebar_progress)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(palette().surface)
                        .inner_margin(SIDEBAR_PADDING),
                )
                .show(root_ui, |ui| self.sidebar(ui));
        }
        if assistant_consumes_width {
            egui::Panel::right("reader-assistant")
                .exact_size(self.ui.assistant_width * assistant_progress)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(palette().background)
                        .inner_margin(egui::Margin::symmetric(ASSISTANT_SIDE_PADDING, 0)),
                )
                .show(root_ui, |ui| self.assistant(ui));
        }
        (sidebar_progress, assistant_progress)
    }

    fn resize_side_panels(
        &mut self,
        ctx: &egui::Context,
        sidebar_progress: f32,
        assistant_progress: f32,
        interaction_blocked: bool,
    ) {
        if interaction_blocked {
            return;
        }
        let viewport = ctx.content_rect();
        let assistant_visible = !self.is_focus_mode()
            && self.ui.assistant_panel.is_some()
            && assistant_progress > 0.001;
        if sidebar_progress >= 0.999 {
            let assistant_reservation = if self.ui.sidebar_pinned && assistant_visible {
                self.ui.assistant_width
            } else {
                0.0
            };
            let sidebar_max = (viewport.width() - MIN_READER_CONTENT_WIDTH - assistant_reservation)
                .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
            if let Some(pointer_x) = panel_resize_pointer(
                ctx,
                "reader-sidebar-resize",
                viewport.left() + self.ui.sidebar_width,
            ) {
                self.ui.sidebar_width =
                    (pointer_x - viewport.left()).clamp(SIDEBAR_MIN_WIDTH, sidebar_max);
                ctx.request_repaint();
            }
        }
        if assistant_visible && assistant_progress >= 0.999 {
            let sidebar_reservation = if self.ui.sidebar_pinned && sidebar_progress > 0.001 {
                self.ui.sidebar_width
            } else {
                0.0
            };
            let assistant_max = (viewport.width() - MIN_READER_CONTENT_WIDTH - sidebar_reservation)
                .clamp(ASSISTANT_MIN_WIDTH, ASSISTANT_MAX_WIDTH);
            if let Some(pointer_x) = panel_resize_pointer(
                ctx,
                "reader-assistant-resize",
                viewport.right() - self.ui.assistant_width,
            ) {
                self.ui.assistant_width =
                    (viewport.right() - pointer_x).clamp(ASSISTANT_MIN_WIDTH, assistant_max);
                ctx.request_repaint();
            }
        }
    }

    fn keyboard_shortcuts(&mut self, ctx: &egui::Context, interaction_blocked: bool) {
        let focus_footnote_requested =
            self.focus_footnote_shortcut_requested(ctx, interaction_blocked);
        let escape_pressed = self.ui.overlay == ReaderOverlay::Menu
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if reader_menu_close_requested(self.ui.overlay, escape_pressed) {
            self.close_overlay();
            return;
        }
        if self.is_focus_mode()
            && self.ui.focus_footnotes_visible
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.close_focus_footnotes();
            return;
        }
        if self.is_focus_mode()
            && !interaction_blocked
            && self.ui.focus_actions_visible
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.ui.focus_actions_visible = false;
            self.annotation_note_draft = None;
            ctx.memory_mut(egui::Memory::stop_text_input);
            return;
        }
        if !interaction_blocked
            && self.ui.sidebar_open
            && !self.ui.sidebar_pinned
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.set_sidebar_open(false);
            ctx.memory_mut(egui::Memory::stop_text_input);
            return;
        }
        if self.focus_footnote_shortcut(ctx, focus_footnote_requested) {
            return;
        }
        if self.ui.focus_footnotes_visible {
            let scroll_delta = ctx.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    -ASSISTANT_KEYBOARD_SCROLL_STEP
                } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    ASSISTANT_KEYBOARD_SCROLL_STEP
                } else {
                    0.0
                }
            });
            if scroll_delta != 0.0 {
                self.ui.focus_footnote_scroll_delta += scroll_delta;
                ctx.request_repaint();
            }
            return;
        }
        if self.is_focus_mode()
            && !interaction_blocked
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            && self.ui.assistant_panel.is_some()
        {
            self.close_assistant_panel();
            ctx.memory_mut(egui::Memory::stop_text_input);
            return;
        }
        if self.is_focus_mode()
            && !interaction_blocked
            && self.ui.assistant_panel.is_some()
            && self.current_chat_has_data()
            && !assistant_suggestions_active(
                &self.chat.input,
                self.chat.cursor_char_index,
                &self.chat.references,
            )
        {
            let scroll_delta = ctx.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    -ASSISTANT_KEYBOARD_SCROLL_STEP
                } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    ASSISTANT_KEYBOARD_SCROLL_STEP
                } else {
                    0.0
                }
            });
            if scroll_delta != 0.0 {
                self.chat.pending_keyboard_scroll_delta += scroll_delta;
                ctx.request_repaint();
                return;
            }
        }
        if self.toc_keyboard_shortcut(ctx, interaction_blocked) {
            return;
        }
        if self.operation_shortcut(ctx, interaction_blocked) {
            return;
        }
        if self.layout_shortcut(ctx, interaction_blocked) {
            return;
        }
        if self.focus_action_shortcut(ctx, interaction_blocked) {
            return;
        }
        // Focus-mode reading shortcuts are handled before the generic keyboard-focus
        // guard so a stale TextEdit focus cannot intermittently swallow them.
        if self.focus_body_accepts_shortcuts(interaction_blocked)
            && ctx.input_mut(|input| input.consume_shortcut(&self.shortcuts.focus_actions))
        {
            self.ui.focus_actions_visible = true;
            ctx.memory_mut(egui::Memory::stop_text_input);
            return;
        }
        if self.focus_body_accepts_shortcuts(interaction_blocked)
            && ctx.input_mut(|input| input.consume_shortcut(&self.shortcuts.focus_chat))
        {
            self.ui.focus_actions_visible = false;
            self.attach_current_focus_reference();
            self.open_assistant_panel(AssistantPanel::Chat);
            return;
        }
        let open_search = !interaction_blocked
            && !self.ui.overlay_visible()
            && ctx.input_mut(|input| input.consume_shortcut(&self.shortcuts.search));
        if open_search {
            self.open_search();
            return;
        }
        let visible_focus_editor = self.is_focus_mode()
            && (self.ui.assistant_panel.is_some() || self.annotation_note_draft.is_some());
        if interaction_blocked
            || (ctx.egui_wants_keyboard_input()
                && (!self.is_focus_mode() || visible_focus_editor || self.ui.sidebar_open))
            || self.ui.overlay_visible()
            || self.image_preview.is_some()
        {
            return;
        }
        // The focus-mode sidebar is a floating interaction layer. While it is open,
        // navigation keys belong to its scrollable content and must never advance the
        // active paragraph or section underneath it.
        if self.is_focus_mode() && self.ui.sidebar_open {
            return;
        }
        self.reading_navigation_shortcuts(ctx);
    }

    fn toc_keyboard_shortcut(&mut self, ctx: &egui::Context, interaction_blocked: bool) -> bool {
        if interaction_blocked
            || !self.ui.sidebar_open
            || self.ui.sidebar_tab != SidebarTab::Toc
            || !toc_keyboard_navigation_enabled(self.is_focus_mode(), self.ui.sidebar_pinned)
            || self.ui.overlay_visible()
            || self.image_preview.is_some()
            || self.annotation_note_draft.is_some()
            || ctx.text_edit_focused()
        {
            return false;
        }
        let action = ctx.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                Some(TocKeyboardAction::Previous)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                Some(TocKeyboardAction::Next)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                Some(TocKeyboardAction::Expand)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                Some(TocKeyboardAction::Collapse)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                Some(TocKeyboardAction::Activate)
            } else {
                None
            }
        });
        let Some(action) = action else {
            return false;
        };
        let row_indices = self.visible_toc_row_indices();
        let active_row = self.snapshot.active_toc_id.as_ref().and_then(|active| {
            row_indices.iter().position(|&index| {
                self.reader.toc_items().get(index).map(|row| &row.id) == Some(active)
            })
        });
        match action {
            TocKeyboardAction::Previous | TocKeyboardAction::Next => {
                self.ui.toc_keyboard_row = next_toc_keyboard_row(
                    self.ui.toc_keyboard_row,
                    active_row,
                    row_indices.len(),
                    if action == TocKeyboardAction::Previous {
                        PageDirection::Previous
                    } else {
                        PageDirection::Next
                    },
                );
            }
            TocKeyboardAction::Expand | TocKeyboardAction::Collapse => {
                let focused_row = self
                    .ui
                    .toc_keyboard_row
                    .filter(|&row| row < row_indices.len())
                    .or(active_row);
                self.ui.toc_keyboard_row = focused_row;
                let item = focused_row
                    .and_then(|row| row_indices.get(row))
                    .and_then(|&index| self.reader.toc_items().get(index))
                    .map(|item| {
                        (
                            item.id.clone(),
                            item.has_children,
                            self.ui.expanded_toc.contains(&item.id),
                        )
                    });
                if let Some((id, has_children, expanded)) = item
                    && let Some(target) = toc_expansion_target(action, has_children, expanded)
                {
                    if target {
                        self.ui.expanded_toc.insert(id);
                    } else {
                        self.ui.expanded_toc.remove(&id);
                    }
                }
            }
            TocKeyboardAction::Activate => {
                let focused_row = self
                    .ui
                    .toc_keyboard_row
                    .filter(|&row| row < row_indices.len())
                    .or(active_row);
                self.ui.toc_keyboard_row = focused_row;
                let target = focused_row
                    .and_then(|row| row_indices.get(row))
                    .and_then(|&index| self.reader.toc_items().get(index))
                    .and_then(|item| {
                        item.target
                            .as_ref()
                            .map(|target| (item.id.clone(), target.clone()))
                    });
                if let Some((id, target)) = target {
                    self.go_to_toc(&id, &target);
                }
            }
        }
        ctx.request_repaint();
        true
    }

    fn focus_footnote_shortcut_requested(
        &mut self,
        ctx: &egui::Context,
        interaction_blocked: bool,
    ) -> bool {
        let context_active = self.is_focus_mode()
            && !interaction_blocked
            && !self.ui.overlay_visible()
            && !self.ui.sidebar_open
            && self.image_preview.is_none()
            && self.ui.assistant_panel.is_none()
            && self.annotation_note_draft.is_none()
            && !ctx.text_edit_focused();
        if !context_active {
            self.ui.focus_footnote_modifier_tap = ModifierTapState::Idle;
            return false;
        }

        let shortcut = self.shortcuts.focus_footnotes;
        if !is_bare_left_alt_shortcut(shortcut) {
            self.ui.focus_footnote_modifier_tap = ModifierTapState::Idle;
            return ctx.input_mut(|input| input.consume_shortcut(&shortcut));
        }

        ctx.input(|input| {
            modifier_tap_triggered(
                &mut self.ui.focus_footnote_modifier_tap,
                &input.raw.events,
                shortcut.logical_key,
            )
        })
    }

    fn focus_footnote_shortcut(&mut self, ctx: &egui::Context, requested: bool) -> bool {
        if !requested {
            return false;
        }
        if self.ui.focus_footnotes_visible {
            self.close_focus_footnotes();
            return true;
        }
        let can_open = self
            .focus_units
            .get(self.focus_unit_index)
            .is_some_and(|unit| !unit.footnotes.is_empty());
        if can_open {
            self.ui.focus_footnotes_visible = true;
            self.ui.focus_footnote_scroll_delta = 0.0;
            self.ui.focus_actions_visible = false;
            self.cancel_text_selection();
            ctx.memory_mut(egui::Memory::stop_text_input);
            ctx.request_repaint();
        }
        true
    }

    fn close_focus_footnotes(&mut self) {
        self.ui.focus_footnotes_visible = false;
        self.ui.focus_footnote_scroll_delta = 0.0;
        self.classic_footnotes.clear();
        self.classic_footnote_anchor_y = None;
        self.classic_footnote_overlay_rect = None;
    }

    fn operation_shortcut(&mut self, ctx: &egui::Context, interaction_blocked: bool) -> bool {
        if interaction_blocked
            || self.ui.overlay_visible()
            || self.image_preview.is_some()
            || self.annotation_note_draft.is_some()
            || ctx.text_edit_focused()
        {
            return false;
        }
        let cursor_toggle_allowed = self.is_focus_mode();
        let action = ctx.input_mut(|input| {
            if input.consume_shortcut(&self.shortcuts.toggle_translation) {
                Some(0)
            } else if input.consume_shortcut(&self.shortcuts.return_to_shelf) {
                Some(1)
            } else if cursor_toggle_allowed && input.consume_shortcut(&self.shortcuts.toggle_cursor)
            {
                Some(2)
            } else {
                None
            }
        });
        match action {
            Some(0) => self.toggle_translation(),
            Some(1) => self.request_exit(),
            Some(2) => self.toggle_focus_cursor_visibility(),
            Some(_) => unreachable!(),
            None => return false,
        }
        true
    }

    fn layout_shortcut(&mut self, ctx: &egui::Context, interaction_blocked: bool) -> bool {
        if interaction_blocked
            || self.ui.overlay_visible()
            || self.image_preview.is_some()
            || self.annotation_note_draft.is_some()
            || ctx.text_edit_focused()
        {
            return false;
        }
        let action = ctx.input_mut(|input| {
            if input.consume_shortcut(&self.shortcuts.toggle_left_sidebar) {
                Some(0)
            } else if input.consume_shortcut(&self.shortcuts.toggle_right_sidebar) {
                Some(1)
            } else {
                None
            }
        });
        match action {
            Some(0) => self.set_sidebar_open(!self.ui.sidebar_open),
            Some(1) => {
                if self.is_focus_mode() && self.ui.assistant_motion.target <= 0.5 {
                    self.attach_current_focus_reference();
                }
                self.toggle_assistant_panel(AssistantPanel::Chat);
            }
            Some(_) => unreachable!(),
            None => return false,
        }
        true
    }

    fn reading_navigation_shortcuts(&mut self, ctx: &egui::Context) {
        if self.is_focus_mode() {
            let (
                extend_previous,
                extend_next,
                previous_unit,
                previous_unit_fresh,
                next_unit,
                next_unit_fresh,
                previous_section,
                next_section,
            ) = ctx.input_mut(|input| {
                let previous_unit_fresh =
                    shortcut_has_fresh_press(input, &self.shortcuts.previous_page_or_paragraph);
                let next_unit_fresh =
                    shortcut_has_fresh_press(input, &self.shortcuts.next_page_or_paragraph);
                (
                    input.consume_shortcut(&self.shortcuts.focus_extend_selection_previous),
                    input.consume_shortcut(&self.shortcuts.focus_extend_selection_next),
                    input.consume_shortcut(&self.shortcuts.previous_page_or_paragraph),
                    previous_unit_fresh,
                    input.consume_shortcut(&self.shortcuts.next_page_or_paragraph),
                    next_unit_fresh,
                    input.consume_shortcut(&self.shortcuts.previous_section),
                    input.consume_shortcut(&self.shortcuts.next_section),
                )
            });
            if extend_previous {
                self.extend_focus_selection(PageDirection::Previous);
            } else if extend_next {
                self.extend_focus_selection(PageDirection::Next);
            } else if previous_unit {
                self.cancel_text_selection();
                if !self.scroll_within_tall_focus_unit(PageDirection::Previous)
                    && previous_unit_fresh
                {
                    self.move_focus_unit(PageDirection::Previous);
                }
            } else if next_unit {
                self.cancel_text_selection();
                if !self.scroll_within_tall_focus_unit(PageDirection::Next) && next_unit_fresh {
                    self.move_focus_unit(PageDirection::Next);
                }
            } else if previous_section {
                self.go_to_adjacent_section(PageDirection::Previous);
            } else if next_section {
                self.go_to_adjacent_section(PageDirection::Next);
            }
            return;
        }
        if self.is_scroll_mode() {
            self.scroll_navigation_shortcuts(ctx);
            return;
        }
        let action = ctx.input_mut(|input| {
            let left = input.consume_shortcut(&self.shortcuts.previous_section);
            let up = input.consume_shortcut(&self.shortcuts.previous_page_or_paragraph);
            let page_up = input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp);
            let right = input.consume_shortcut(&self.shortcuts.next_section);
            let down = input.consume_shortcut(&self.shortcuts.next_page_or_paragraph);
            let page_down = input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown);
            let space = input.consume_key(egui::Modifiers::NONE, egui::Key::Space);
            classic_navigation_action(left, right, up, down, page_up, page_down, space)
        });
        match action {
            Some(ClassicNavigationAction::PreviousReadingUnit) => {
                self.go_to_adjacent_section(PageDirection::Previous);
            }
            Some(ClassicNavigationAction::NextReadingUnit) => {
                self.go_to_adjacent_section(PageDirection::Next);
            }
            Some(ClassicNavigationAction::PreviousPage) => {
                self.turn_page(PageDirection::Previous);
            }
            Some(ClassicNavigationAction::NextPage) => {
                self.turn_page(PageDirection::Next);
            }
            None => {}
        }
    }

    fn scroll_navigation_shortcuts(&mut self, ctx: &egui::Context) {
        let (previous, next, up, down, page_up, page_down) = ctx.input_mut(|input| {
            (
                input.consume_shortcut(&self.shortcuts.previous_section),
                input.consume_shortcut(&self.shortcuts.next_section),
                input.consume_shortcut(&self.shortcuts.previous_page_or_paragraph),
                input.consume_shortcut(&self.shortcuts.next_page_or_paragraph),
                input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
            )
        });
        if previous || next {
            self.go_to_adjacent_section(if previous {
                PageDirection::Previous
            } else {
                PageDirection::Next
            });
        } else if up || page_up {
            self.scroll_with_keyboard(PageDirection::Previous, page_up);
        } else if down || page_down {
            self.scroll_with_keyboard(PageDirection::Next, page_down);
        }
    }

    fn scroll_with_keyboard(&mut self, direction: PageDirection, page_step: bool) {
        let at_boundary = match direction {
            PageDirection::Previous => self
                .scroll_viewport
                .is_some_and(|viewport| viewport.offset_y <= MOTION_EPSILON),
            PageDirection::Next => self
                .scroll_section
                .as_ref()
                .zip(self.scroll_viewport)
                .is_some_and(|(layout, viewport)| {
                    let padding = self.scroll_content_padding(viewport.size.y);
                    let max_offset =
                        (layout.content_height + padding * 2.0 - viewport.size.y).max(0.0);
                    viewport.offset_y >= max_offset - MOTION_EPSILON
                }),
        };
        if at_boundary {
            self.go_to_adjacent_section(direction);
            return;
        }
        let step = self.scroll_viewport.map_or(48.0, |viewport| {
            if page_step {
                viewport.size.y * 0.8
            } else {
                48.0
            }
        });
        self.pending_keyboard_scroll_delta = match direction {
            PageDirection::Previous => step,
            PageDirection::Next => -step,
        };
    }

    fn focus_action_shortcut(&mut self, ctx: &egui::Context, interaction_blocked: bool) -> bool {
        if !self.focus_body_accepts_shortcuts(interaction_blocked) {
            return false;
        }
        let action = ctx.input_mut(|input| {
            if input.consume_shortcut(&self.shortcuts.focus_highlight) {
                Some(0)
            } else if input.consume_shortcut(&self.shortcuts.focus_note) {
                Some(1)
            } else if input.consume_shortcut(&self.shortcuts.focus_structure) {
                Some(2)
            } else {
                None
            }
        });
        match action {
            Some(0) if self.focus_has_annotatable_units() => {
                self.ui.focus_actions_visible = false;
                self.create_focus_highlight(None);
            }
            Some(1) if self.focus_has_annotatable_units() => {
                self.ui.focus_actions_visible = true;
                self.annotation_note_draft = Some(AnnotationDraft {
                    note: self.current_focus_note().unwrap_or_default(),
                    focus_pending: true,
                });
            }
            Some(2) if self.focus_has_structurable_units() => {
                self.ui.focus_actions_visible = false;
                self.toggle_current_focus_structure();
            }
            Some(_) => {}
            None => return false,
        }
        true
    }

    fn focus_body_accepts_shortcuts(&self, interaction_blocked: bool) -> bool {
        self.is_focus_mode()
            && !interaction_blocked
            && !self.ui.overlay_visible()
            && !self.ui.sidebar_open
            && self.image_preview.is_none()
            && self.ui.assistant_panel.is_none()
            && self.annotation_note_draft.is_none()
            && !self.ui.focus_footnotes_visible
    }

    fn focus_wheel_interaction(&mut self, response: &egui::Response) {
        if self.ui.focus_footnotes_visible
            || self.ui.sidebar_open
            || self.ui.assistant_panel.is_some() && self.current_chat_has_data()
        {
            self.ui.wheel_accumulator = 0.0;
            return;
        }
        let delta = response.ctx.input(|input| {
            input
                .raw
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } if !modifiers.ctrl && !modifiers.command => Some(
                        delta.y
                            * match unit {
                                egui::MouseWheelUnit::Point => 1.0,
                                egui::MouseWheelUnit::Line => 50.0,
                                egui::MouseWheelUnit::Page => 240.0,
                            },
                    ),
                    _ => None,
                })
                .sum::<f32>()
        });
        if delta.abs() <= f32::EPSILON {
            return;
        }
        if self.ui.wheel_accumulator.signum() != delta.signum() {
            self.ui.wheel_accumulator = 0.0;
        }
        self.ui.wheel_accumulator += delta;
        if self.ui.wheel_accumulator.abs() < WHEEL_PAGE_THRESHOLD {
            return;
        }
        let now = Instant::now();
        if self
            .ui
            .last_wheel_turn
            .is_some_and(|last| now.saturating_duration_since(last) < WHEEL_TURN_COOLDOWN)
        {
            return;
        }
        let direction = if self.ui.wheel_accumulator < 0.0 {
            PageDirection::Next
        } else {
            PageDirection::Previous
        };
        self.ui.wheel_accumulator = 0.0;
        self.ui.last_wheel_turn = Some(now);
        // This callback runs after scroll_content has already chosen the current
        // layout and painted its texture. Applying a cross-unit navigation here
        // lets the GPU observe the new reading unit before its focus units and
        // target offset are rebuilt, producing a one-frame flash of the new
        // section's first image. Defer the turn to the beginning of the next UI
        // frame, where it follows the same state order as keyboard navigation.
        self.pending_focus_wheel_turn = Some(direction);
        response.ctx.request_repaint();
        response.ctx.input_mut(|input| {
            input.smooth_scroll_delta.y = 0.0;
        });
    }

    fn scroll_boundary_wheel_interaction(
        &mut self,
        response: &egui::Response,
        interaction_blocked: bool,
    ) {
        let pointer_over_page = response
            .ctx
            .pointer_hover_pos()
            .is_some_and(|position| response.rect.contains(position));
        if interaction_blocked
            || !pointer_over_page
            || self.ui.overlay_visible()
            || self.ui.sidebar_open && !self.ui.sidebar_pinned
        {
            self.ui.wheel_accumulator = 0.0;
            return;
        }
        let delta = response.ctx.input(|input| {
            input
                .raw
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } if !modifiers.ctrl && !modifiers.command => Some(
                        delta.y
                            * match unit {
                                egui::MouseWheelUnit::Point => 1.0,
                                egui::MouseWheelUnit::Line => 50.0,
                                egui::MouseWheelUnit::Page => 240.0,
                            },
                    ),
                    _ => None,
                })
                .sum::<f32>()
        });
        let Some((layout, viewport)) = self.scroll_section.as_ref().zip(self.scroll_viewport)
        else {
            return;
        };
        let max_offset = (layout.content_height - viewport.size.y).max(0.0);
        let direction = if delta > 0.0 && viewport.offset_y <= MOTION_EPSILON {
            Some(PageDirection::Previous)
        } else if delta < 0.0 && viewport.offset_y >= max_offset - MOTION_EPSILON {
            Some(PageDirection::Next)
        } else {
            None
        };
        let Some(direction) = direction else {
            self.ui.wheel_accumulator = 0.0;
            return;
        };
        if self.ui.wheel_accumulator.signum() != delta.signum() {
            self.ui.wheel_accumulator = 0.0;
        }
        self.ui.wheel_accumulator += delta;
        if self.ui.wheel_accumulator.abs() < WHEEL_PAGE_THRESHOLD {
            return;
        }
        let now = Instant::now();
        if self
            .ui
            .last_wheel_turn
            .is_some_and(|last| now.saturating_duration_since(last) < WHEEL_TURN_COOLDOWN)
        {
            return;
        }
        self.ui.wheel_accumulator = 0.0;
        self.ui.last_wheel_turn = Some(now);
        self.go_to_adjacent_section(direction);
        response.ctx.input_mut(|input| {
            input.smooth_scroll_delta.y = 0.0;
        });
    }

    fn wheel_interaction(&mut self, response: &egui::Response, interaction_blocked: bool) {
        let pointer_over_page = response
            .ctx
            .pointer_hover_pos()
            .is_some_and(|position| response.rect.contains(position));
        let blocked = interaction_blocked
            || self.ui.overlay_visible()
            || self.image_preview.is_some()
            || self.pending_page_turn.is_some();
        if !page_wheel_input_allowed(pointer_over_page, blocked) {
            self.ui.wheel_accumulator = 0.0;
            return;
        }

        let delta = response.ctx.input(|input| {
            input
                .raw
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } if !modifiers.ctrl && !modifiers.command => Some(
                        delta.y
                            * match unit {
                                egui::MouseWheelUnit::Point => 1.0,
                                egui::MouseWheelUnit::Line => 50.0,
                                egui::MouseWheelUnit::Page => 240.0,
                            },
                    ),
                    _ => None,
                })
                .sum::<f32>()
        });
        if delta.abs() <= f32::EPSILON {
            return;
        }
        if self.ui.wheel_accumulator.signum() != delta.signum() {
            self.ui.wheel_accumulator = 0.0;
        }
        self.ui.wheel_accumulator += delta;
        if self.ui.wheel_accumulator.abs() < WHEEL_PAGE_THRESHOLD {
            return;
        }

        let now = Instant::now();
        if self
            .ui
            .last_wheel_turn
            .is_some_and(|last| now.saturating_duration_since(last) < WHEEL_TURN_COOLDOWN)
        {
            return;
        }
        let direction = if self.ui.wheel_accumulator < 0.0 {
            PageDirection::Next
        } else {
            PageDirection::Previous
        };
        self.ui.wheel_accumulator = 0.0;
        self.ui.last_wheel_turn = Some(now);
        response.ctx.input_mut(|input| {
            input.smooth_scroll_delta.y = 0.0;
        });
        self.turn_page(direction);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, background: Color32) {
        let toolbar_width = ui.available_width();
        let hover_rect = Rect::from_min_size(
            ui.next_widget_position(),
            Vec2::new(toolbar_width, TOOLBAR_HEIGHT),
        );
        let content_left = self.toolbar_content_left(toolbar_width);
        let toolbar_actions_visible = self.ui.toolbar_motion.value.clamp(0.0, 1.0) > 0.02
            || self.ui.overlay == ReaderOverlay::Menu;
        let chapter_title = self.current_chapter_title().to_owned();
        egui::Frame::new()
            .fill(background)
            .inner_margin(egui::Margin::symmetric(0, SIDEBAR_PADDING))
            .show(ui, |ui| {
                ui.set_min_width(toolbar_width);
                ui.set_min_height(TOOLBAR_HEIGHT - f32::from(SIDEBAR_PADDING) * 2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let left_control_count = if self.ui.sidebar_open { 2.0 } else { 3.0 }
                        + self.pdf_ocr_toolbar_control_count();
                    let left_controls_width = TOOLBAR_CONTROL_SIZE * left_control_count;
                    // Keep the first toolbar action clear of the sidebar divider.
                    // Only the spacer after the action group may collapse on narrow layouts.
                    let button_left = f32::from(SIDEBAR_PADDING);
                    ui.add_space(button_left);
                    if toolbar_actions_visible {
                        if !self.ui.sidebar_open
                            && icon_button(ui, Icon::PanelLeft)
                                .on_hover_text(self.language.text("展开侧栏", "Open sidebar"))
                                .clicked()
                        {
                            self.set_sidebar_open(true);
                        }
                        if icon_button(ui, Icon::Library)
                            .on_hover_text(self.language.text("返回书架", "Back to library"))
                            .clicked()
                        {
                            self.request_exit();
                        }
                        if selectable_icon_button(ui, Icon::Languages, self.translation.enabled)
                            .on_hover_text(if self.translation.enabled {
                                self.language.text("关闭翻译", "Turn translation off")
                            } else {
                                self.language.text("开启翻译", "Turn translation on")
                            })
                            .clicked()
                        {
                            self.toggle_translation();
                        }
                        self.pdf_ocr_toolbar_controls(ui);
                    } else {
                        ui.allocate_space(Vec2::new(left_controls_width, TOOLBAR_CONTROL_SIZE));
                    }
                    ui.add_space((content_left - button_left - left_controls_width).max(0.0));
                    if toolbar_actions_visible {
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .id_salt("reader-toolbar-actions")
                                .layout(egui::Layout::right_to_left(egui::Align::Center)),
                            |ui| {
                                ui.add_space(12.0);
                                if icon_button(ui, Icon::Menu)
                                    .on_hover_text(self.language.text("菜单", "Menu"))
                                    .clicked()
                                {
                                    self.toggle_menu();
                                }
                                if !self.is_focus_mode()
                                    && icon_button(ui, Icon::MessageCircle)
                                        .on_hover_text(
                                            self.language.text("AI 助手", "AI assistant"),
                                        )
                                        .clicked()
                                {
                                    self.toggle_assistant_panel(AssistantPanel::Chat);
                                }
                            },
                        );
                    }
                });
            });
        if toolbar_actions_visible {
            paint_toolbar_title(ui, hover_rect, content_left, true, &chapter_title);
        }
        let hovered = ui.ctx().input(|input| {
            input
                .pointer
                .hover_pos()
                .is_some_and(|position| hover_rect.contains(position))
        });
        if self.set_toolbar_hovered(hovered) {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
    }

    fn pdf_ocr_toolbar_control_count(&self) -> f32 {
        if self.format != rebook_formats::BookFormat::Pdf {
            return 0.0;
        }
        let recognize = if self.plugin_settings.pdf_ocr_enabled {
            1.0
        } else {
            0.0
        };
        let switch = if self.pdf_ocr.available { 1.0 } else { 0.0 };
        recognize + switch
    }

    fn pdf_ocr_toolbar_controls(&mut self, ui: &mut egui::Ui) {
        if self.format != rebook_formats::BookFormat::Pdf {
            return;
        }
        if self.plugin_settings.pdf_ocr_enabled {
            let pending = self.pdf_ocr.task.is_pending();
            let label = if self.pdf_ocr.available {
                self.language
                    .text("重新识别 PDF 正文", "Recognize PDF text again")
            } else {
                self.language.text("识别 PDF 正文", "Recognize PDF text")
            };
            let recognize = ui.add_enabled_ui(!pending, |ui| icon_button(ui, Icon::ScanText));
            let recognize = if pending {
                recognize
                    .inner
                    .on_disabled_hover_text(self.pdf_ocr.progress.as_str())
            } else {
                recognize.inner.on_hover_text(label)
            };
            if recognize.clicked() {
                self.start_pdf_ocr();
            }
        }
        if self.pdf_ocr.available
            && selectable_icon_button(ui, Icon::Type, self.pdf_ocr.mode == PdfOcrViewMode::Reflow)
                .on_hover_text(if self.pdf_ocr.mode == PdfOcrViewMode::Reflow {
                    self.language.text("切换到原始 PDF", "Show original PDF")
                } else {
                    self.language.text("切换到 OCR 版式", "Show OCR reflow")
                })
                .clicked()
            && self.toggle_pdf_ocr_view()
        {
            // The reopen request is consumed at the beginning of the next UI frame.
            // Explicitly wake it so switching does not wait for another input event.
            ui.ctx().request_repaint();
        }
    }

    fn toolbar_content_left(&mut self, toolbar_width: f32) -> f32 {
        let default = reading_content_left(toolbar_width, &self.reader.style());
        if !uses_pdf_page_alignment(self.format, self.pdf_ocr.mode) {
            return default;
        }
        self.reader
            .current_spread()
            .ok()
            .and_then(|spread| {
                page_image_left(&spread.primary).map(|left| left + spread.primary_offset_x)
            })
            .filter(|left| (0.0..toolbar_width).contains(left))
            .unwrap_or(default)
    }

    fn current_chapter_title(&self) -> &str {
        let Some(active_id) = self.snapshot.active_toc_id.as_deref() else {
            return &self.display_metadata.title;
        };
        if self.translation.enabled
            && self.plugin_settings.translate_toc
            && let Some(label) = self.translation.toc_labels.get(active_id)
        {
            return label;
        }
        self.reader
            .toc_items()
            .iter()
            .find(|item| item.id == active_id)
            .map_or(&self.display_metadata.title, |item| item.label.as_str())
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if icon_button(ui, Icon::PanelLeft)
                .on_hover_text(self.language.text("收起侧栏", "Close sidebar"))
                .clicked()
            {
                self.set_sidebar_open(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.is_focus_mode()
                    && icon_button(
                        ui,
                        if self.ui.sidebar_pinned {
                            Icon::Pin
                        } else {
                            Icon::PinOff
                        },
                    )
                    .on_hover_text(if self.ui.sidebar_pinned {
                        self.language.text("取消固定", "Unpin sidebar")
                    } else {
                        self.language.text("固定侧栏", "Pin sidebar")
                    })
                    .clicked()
                {
                    self.ui.sidebar_pinned = !self.ui.sidebar_pinned;
                }

                if selectable_icon_button(
                    ui,
                    Icon::ListTree,
                    self.ui.sidebar_tab == SidebarTab::Toc,
                )
                .on_hover_text(self.language.text("目录", "Contents"))
                .clicked()
                {
                    self.set_sidebar_tab(SidebarTab::Toc);
                }
                if selectable_icon_button(
                    ui,
                    Icon::MessageSquareText,
                    self.ui.sidebar_tab == SidebarTab::Highlights,
                )
                .on_hover_text(self.language.text("高亮与批注", "Highlights & notes"))
                .clicked()
                {
                    self.set_sidebar_tab(SidebarTab::Highlights);
                }
                if selectable_icon_button(
                    ui,
                    Icon::Search,
                    self.ui.sidebar_tab == SidebarTab::Search,
                )
                .on_hover_text(self.language.text("搜索", "Search"))
                .clicked()
                {
                    self.open_search();
                }
            });
        });
        self.book_summary(ui);
        ui.separator();
        ui.add_space(4.0);
        match self.ui.sidebar_tab {
            SidebarTab::Toc => self.toc(ui),
            SidebarTab::Highlights => self.highlights(ui),
            SidebarTab::Search => self.search(ui),
        }
    }

    fn floating_sidebar(&mut self, ctx: &egui::Context, progress: f32) {
        let screen = ctx.content_rect();
        egui::Area::new("reader-sidebar-scrim".into())
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, Color32::BLACK.gamma_multiply(0.31 * progress));
                if response.clicked() {
                    self.set_sidebar_open(false);
                }
            });
        egui::Area::new("reader-sidebar-floating".into())
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(-self.ui.sidebar_width * (1.0 - progress), 0.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(palette().surface)
                    .stroke(egui::Stroke::new(1.0, palette().border))
                    .inner_margin(SIDEBAR_PADDING)
                    .show(ui, |ui| {
                        let sidebar_inset = f32::from(SIDEBAR_PADDING) * 2.0;
                        ui.set_width(self.ui.sidebar_width - sidebar_inset);
                        ui.set_height(ctx.content_rect().height() - sidebar_inset);
                        self.sidebar(ui);
                    });
            });
    }

    fn focus_assistant_overlay(&mut self, ctx: &egui::Context, page_rect: Rect) {
        self.sync_focus_chat_session();
        let viewport = ctx.content_rect();
        let style = self.reader.style();
        let content_right = page_rect.left()
            + reading_content_left(page_rect.width(), &style)
            + reading_content_width(page_rect.width(), &style);
        self.focus_data_indicator_overlays(ctx, page_rect, viewport, content_right);
        if self.ui.assistant_panel.is_none() {
            return;
        }

        let anchor_y = self
            .focused_unit_screen_center_y(page_rect)
            .unwrap_or_else(|| page_rect.center().y);
        let x = content_right + 12.0;
        let width = (viewport.right() - x - 16.0).clamp(120.0, 420.0);
        let has_conversation = self.current_chat_has_data() || self.chat.error.is_some();
        let (content_height, frame_margin) = if has_conversation {
            (
                (viewport.height() * 0.58).clamp(280.0, 520.0),
                egui::Margin::same(12),
            )
        } else {
            (ASSISTANT_INPUT_HEIGHT, egui::Margin::symmetric(10, 6))
        };
        let height = content_height + f32::from(frame_margin.top) + f32::from(frame_margin.bottom);
        let y = (anchor_y - height / 2.0).clamp(
            viewport.top() + 16.0,
            (viewport.bottom() - height - 16.0).max(viewport.top() + 16.0),
        );
        let dialog = egui::Area::new("focus-assistant".into())
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(x, y))
            .show(ctx, |ui| {
                focus_assistant_frame(frame_margin).show(ui, |ui| {
                    ui.set_width(
                        width - f32::from(frame_margin.left) - f32::from(frame_margin.right),
                    );
                    ui.set_height(content_height);
                    self.focus_assistant_dialog(ui, has_conversation);
                });
            });
        let clicked_outside = ctx.input(|input| {
            input.pointer.any_click()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|position| !dialog.response.rect.contains(position))
        });
        if clicked_outside {
            self.close_assistant_panel();
            ctx.memory_mut(egui::Memory::stop_text_input);
        }
    }

    fn focus_footnote_overlay(&mut self, ctx: &egui::Context, page_rect: Rect) {
        if !self.ui.focus_footnotes_visible {
            return;
        }
        let footnotes = if self.is_focus_mode() {
            self.focus_units
                .get(self.focus_unit_index)
                .map(|unit| unit.footnotes.clone())
                .unwrap_or_default()
        } else {
            self.classic_footnotes.clone()
        };
        if footnotes.is_empty() {
            self.close_focus_footnotes();
            return;
        }

        let viewport = ctx.content_rect();
        let style = self.reader.style();
        let content_right = page_rect.left()
            + reading_content_left(page_rect.width(), &style)
            + reading_content_width(page_rect.width(), &style);
        let preferred_x = content_right + 12.0;
        let maximum_width = 360.0_f32.min((viewport.width() - 32.0).max(1.0));
        let minimum_width = 180.0_f32.min(maximum_width);
        let available_right = viewport.right() - preferred_x - 16.0;
        let width = available_right.clamp(minimum_width, maximum_width);
        let x = if available_right >= minimum_width {
            preferred_x
        } else {
            (viewport.right() - width - 16.0).max(viewport.left() + 16.0)
        };
        let maximum_body_height = (viewport.height() * 0.52).clamp(160.0, 380.0);
        let frame_horizontal_margin = 24.0;
        let leading_icon_width = 19.0;
        let scrollbar_reserve = 8.0;
        let style = ctx.style_of(crate::ui::theme());
        let text_width = (width
            - frame_horizontal_margin
            - leading_icon_width
            - style.spacing.item_spacing.x
            - scrollbar_reserve)
            .max(1.0);
        let body_font = egui::TextStyle::Body.resolve(style.as_ref());
        let text_color = palette().text;
        let footnote_text_layouts = footnotes
            .iter()
            .map(|footnote| {
                optimized_footnote_text_layout(
                    ctx,
                    &footnote.text,
                    &body_font,
                    text_color,
                    text_width,
                )
            })
            .collect::<Vec<_>>();
        let measured_text_height = footnote_text_layouts
            .iter()
            .map(|layout| layout.height)
            .sum::<f32>();
        // Each additional item has one separator plus the vertical spacing on
        // both sides. The small safety inset covers fractional glyph metrics.
        let separator_height = footnotes.len().saturating_sub(1) as f32 * 19.0;
        let measured_body_height = (measured_text_height + separator_height + 2.0)
            .clamp(body_font.size.max(19.0), maximum_body_height);
        let panel_height = measured_body_height + 24.0;
        let anchor_y = if self.is_focus_mode() {
            self.focused_unit_screen_center_y(page_rect)
                .unwrap_or_else(|| page_rect.center().y)
        } else {
            self.classic_footnote_anchor_y
                .unwrap_or_else(|| page_rect.center().y)
        };
        let y = (anchor_y - panel_height / 2.0).clamp(
            viewport.top() + 16.0,
            (viewport.bottom() - panel_height - 16.0).max(viewport.top() + 16.0),
        );
        let keyboard_scroll = std::mem::take(&mut self.ui.focus_footnote_scroll_delta);

        let overlay = egui::Area::new("focus-footnotes".into())
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(x, y))
            .show(ctx, |ui| {
                focus_assistant_frame(egui::Margin::same(12)).show(ui, |ui| {
                    ui.set_width((width - 24.0).max(1.0));
                    let wheel_scroll = ui.input_mut(|input| {
                        let delta = input.smooth_scroll_delta.y;
                        input.smooth_scroll_delta.y = 0.0;
                        delta
                    });
                    let routed_scroll = wheel_scroll - keyboard_scroll;
                    ui.horizontal_top(|ui| {
                        ui.add(icon(Icon::Info).size(19.0).color(footnote_link_color()));
                        let content_width = ui.available_width().max(1.0);
                        ui.vertical(|ui| {
                            ui.set_width(content_width);
                            egui::ScrollArea::vertical()
                                .id_salt("focus-footnotes-scroll")
                                .max_height(maximum_body_height)
                                .min_scrolled_height(measured_body_height)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    if routed_scroll.abs() > f32::EPSILON {
                                        ui.scroll_with_delta(Vec2::new(0.0, routed_scroll));
                                    }
                                    ui.set_width((ui.available_width() - 8.0).max(1.0));
                                    ui.spacing_mut().item_spacing.y = 9.0;
                                    for (index, layout) in footnote_text_layouts.iter().enumerate()
                                    {
                                        if index > 0 {
                                            ui.separator();
                                        }
                                        ui.vertical(|ui| {
                                            ui.spacing_mut().item_spacing.y = 0.0;
                                            for line in &layout.lines {
                                                paint_footnote_text_line(ui, line, text_color);
                                            }
                                        });
                                    }
                                });
                        });
                    });
                });
            });
        if self.is_focus_mode() {
            self.classic_footnote_overlay_rect = None;
        } else {
            self.classic_footnote_overlay_rect = Some(overlay.response.rect);
        }
    }

    fn focus_data_indicator_overlays(
        &mut self,
        ctx: &egui::Context,
        page_rect: Rect,
        viewport: Rect,
        content_right: f32,
    ) {
        let mut clicked_chat_index = None;
        let mut clicked_note = None;
        for index in 0..self.focus_units.len() {
            let current_replacement_visible = index == self.focus_unit_index
                && (self.ui.focus_actions_visible
                    || self.ui.focus_footnotes_visible
                    || self.ui.assistant_panel.is_some());
            if current_replacement_visible {
                continue;
            }
            let has_chat = self.focus_chat_has_data_at(index);
            let note = self.focus_note_at(index);
            if !has_chat && note.is_none() {
                continue;
            }
            let Some(anchor_y) = self.focus_unit_screen_center_y_at(index, page_rect) else {
                continue;
            };
            let group_height = if has_chat && note.is_some() {
                66.0
            } else {
                30.0
            };
            if anchor_y + group_height / 2.0 < viewport.top()
                || anchor_y - group_height / 2.0 > viewport.bottom()
            {
                continue;
            }
            let x = (content_right + 12.0).min(viewport.right() - 44.0);
            let mut y = anchor_y - group_height / 2.0;
            if has_chat {
                let hover_text = self.language.text("打开段落对话", "Open paragraph chat");
                let clicked = egui::Area::new(egui::Id::new(("focus-chat-indicator", index)))
                    .order(egui::Order::Foreground)
                    .fixed_pos(Pos2::new(x, y))
                    .show(ctx, |ui| {
                        focus_data_indicator(ui, Icon::MessageCircle, hover_text)
                    })
                    .inner;
                if clicked {
                    clicked_chat_index = Some(index);
                }
                y += 36.0;
            }
            if let Some(note) = note {
                let hover_text = self.language.text("查看段落批注", "Open paragraph note");
                let clicked = egui::Area::new(egui::Id::new(("focus-note-indicator", index)))
                    .order(egui::Order::Foreground)
                    .fixed_pos(Pos2::new(x, y))
                    .show(ctx, |ui| {
                        focus_data_indicator(ui, Icon::MessageSquareText, hover_text)
                    })
                    .inner;
                if clicked {
                    clicked_note = Some((index, note));
                }
            }
        }
        if let Some(index) = clicked_chat_index {
            self.ui.focus_actions_visible = false;
            self.annotation_note_draft = None;
            self.select_focus_unit(index);
            self.open_assistant_panel(AssistantPanel::Chat);
        } else if let Some((index, note)) = clicked_note {
            self.close_assistant_panel();
            self.select_focus_unit(index);
            self.ui.focus_actions_visible = true;
            self.annotation_note_draft = Some(AnnotationDraft {
                note,
                focus_pending: true,
            });
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the compact focus toolbar keeps its actions and editor transitions together"
    )]
    fn focus_actions_overlay(&mut self, ctx: &egui::Context, page_rect: Rect) {
        if !self.ui.focus_actions_visible {
            return;
        }
        let viewport = ctx.content_rect();
        let anchor_y = self
            .focused_unit_screen_center_y(page_rect)
            .unwrap_or_else(|| page_rect.center().y);
        let style = self.reader.style();
        let content_right = page_rect.left()
            + reading_content_left(page_rect.width(), &style)
            + reading_content_width(page_rect.width(), &style);
        let note_open = self.annotation_note_draft.is_some();
        let x = if note_open {
            content_right + 12.0
        } else {
            (content_right + 12.0).min(viewport.right() - 52.0)
        };
        let width = if note_open {
            (viewport.right() - x - 16.0).clamp(120.0, 338.0)
        } else {
            40.0
        };
        let y = focus_actions_overlay_y(note_open, anchor_y, viewport);
        let mut chat = false;
        let mut highlight = false;
        let mut open_note = false;
        let mut structure = false;
        let mut save_note = false;
        let mut cancel_note = false;
        let can_annotate = self.focus_has_annotatable_units();
        let can_structure = self.focus_has_structurable_units();
        let chat_hover = shortcut_tooltip(
            self.language,
            "聊天",
            "Chat",
            &ctx.format_shortcut(&self.shortcuts.focus_chat),
        );
        let highlight_hover = shortcut_tooltip(
            self.language,
            "高亮",
            "Highlight",
            &ctx.format_shortcut(&self.shortcuts.focus_highlight),
        );
        let note_hover = shortcut_tooltip(
            self.language,
            "添加批注",
            "Add note",
            &ctx.format_shortcut(&self.shortcuts.focus_note),
        );
        let structure_active = self.focus_structure_is_active();
        let structure_hover = shortcut_tooltip(
            self.language,
            if structure_active {
                "恢复原段落"
            } else {
                "按句分段"
            },
            if structure_active {
                "Restore paragraph"
            } else {
                "Split by sentence"
            },
            &ctx.format_shortcut(&self.shortcuts.focus_structure),
        );
        let area = egui::Area::new("focus-actions".into())
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(x, y))
            .show(ctx, |ui| {
                if let Some(draft) = self.annotation_note_draft.as_mut() {
                    focus_assistant_frame(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.set_width((width - 20.0).max(100.0));
                        match focus_annotation_editor(ui, draft, self.language) {
                            AnnotationEditorAction::None => {}
                            AnnotationEditorAction::Save => save_note = true,
                            AnnotationEditorAction::Cancel => cancel_note = true,
                        }
                    });
                } else {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        chat = icon_button(ui, Icon::MessageCircle)
                            .on_hover_text(&chat_hover)
                            .clicked();
                        ui.add_enabled_ui(can_annotate, |ui| {
                            highlight = icon_button(ui, Icon::Highlighter)
                                .on_hover_text(&highlight_hover)
                                .clicked();
                            open_note = icon_button(ui, Icon::MessageSquarePlus)
                                .on_hover_text(&note_hover)
                                .clicked();
                        });
                        structure = ui
                            .add_enabled_ui(can_structure, |ui| {
                                selectable_icon_button(ui, Icon::ListTree, structure_active)
                            })
                            .inner
                            .on_hover_text(&structure_hover)
                            .clicked();
                    });
                }
            });
        if chat {
            self.ui.focus_actions_visible = false;
            self.attach_current_focus_reference();
            self.open_assistant_panel(AssistantPanel::Chat);
        } else if highlight {
            self.ui.focus_actions_visible = false;
            self.create_focus_highlight(None);
        } else if open_note {
            self.annotation_note_draft = Some(AnnotationDraft {
                note: self.current_focus_note().unwrap_or_default(),
                focus_pending: true,
            });
        } else if structure {
            self.ui.focus_actions_visible = false;
            self.toggle_current_focus_structure();
        } else if save_note {
            let note = self
                .annotation_note_draft
                .as_ref()
                .map(|draft| draft.note.clone());
            self.ui.focus_actions_visible = false;
            self.create_focus_highlight(note);
        } else if cancel_note {
            self.annotation_note_draft = None;
        }
        let clicked_outside = ctx.input(|input| {
            input.pointer.any_click()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|position| !area.response.rect.contains(position))
        });
        if clicked_outside {
            self.ui.focus_actions_visible = false;
            self.annotation_note_draft = None;
            ctx.memory_mut(egui::Memory::stop_text_input);
        }
    }

    fn focus_assistant_dialog(&mut self, ui: &mut egui::Ui, has_conversation: bool) {
        let busy = self.chat.task.is_pending();
        if has_conversation {
            let error_height = if self.chat.error.is_some() { 54.0 } else { 0.0 };
            let conversation_height = (ui.available_height()
                - ASSISTANT_COMPOSER_RESERVED_HEIGHT
                - ASSISTANT_BOTTOM_PADDING
                - error_height)
                .max(96.0);
            self.assistant_conversation(ui, conversation_height, busy);
            self.assistant_error(ui);
            self.assistant_annotation_confirmation(ui);
        }
        self.assistant_composer_with_options(ui, false, false, !has_conversation);
    }

    fn focused_unit_screen_center_y(&self, page_rect: Rect) -> Option<f32> {
        self.focus_unit_screen_center_y_at(self.focus_unit_index, page_rect)
    }

    fn focus_unit_screen_center_y_at(&self, index: usize, page_rect: Rect) -> Option<f32> {
        let viewport = self.scroll_viewport?;
        let unit = self.focus_units.get(index)?;
        Some(focus_unit_screen_center_y(
            unit.rect,
            viewport.offset_y,
            self.scroll_content_padding(viewport.size.y),
            page_rect,
        ))
    }

    fn book_summary(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if self.cover_texture.is_none()
                && let Some(bytes) = &self.cover
                && let Ok(image) = decode_color_image(bytes)
            {
                self.cover_texture = Some(ui.ctx().load_texture(
                    "reader-cover",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
            if let Some(texture) = &self.cover_texture {
                ui.add(egui::Image::new(texture).fit_to_exact_size(Vec2::new(52.0, 74.0)));
            } else {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(52.0, 74.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 5.0, palette().surface_muted);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    self.format.label(),
                    egui::FontId::proportional(crate::ui::scaled_font_size(10.0)),
                    palette().accent,
                );
            }
            let summary_width = ui.available_width().max(1.0);
            ui.allocate_ui_with_layout(
                Vec2::new(summary_width, 74.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.label(
                        RichText::new(&self.display_metadata.title)
                            .strong()
                            .color(palette().text),
                    )
                    .on_hover_text(&self.display_metadata.title);
                    let authors = self.display_metadata.authors.join(" / ");
                    if !authors.is_empty() {
                        ui.label(
                            RichText::new(authors)
                                .size(crate::ui::scaled_font_size(12.0))
                                .color(palette().muted),
                        );
                    }
                },
            );
        });
        ui.add_space(10.0);
    }

    fn visible_toc_row_indices(&self) -> Vec<usize> {
        self.reader
            .toc_items()
            .iter()
            .enumerate()
            .filter(|row| {
                row.1
                    .ancestors
                    .iter()
                    .all(|ancestor| self.ui.expanded_toc.contains(ancestor))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn paint_visible_toc_rows(
        &mut self,
        ui: &mut egui::Ui,
        visible_rows: std::ops::Range<usize>,
        row_indices: &[usize],
        active: Option<&String>,
        keyboard_row: Option<usize>,
    ) -> Option<usize> {
        let mut navigated_row = None;
        let content_width = (ui.available_width() - 12.0).max(1.0);
        ui.set_width(content_width);
        for visible_index in visible_rows {
            let row = self.reader.toc_items()[row_indices[visible_index]].clone();
            let selected = active == Some(&row.id);
            let keyboard_focused = keyboard_row == Some(visible_index);
            let display_label = if self.translation.enabled && self.plugin_settings.translate_toc {
                self.translation
                    .toc_labels
                    .get(&row.id)
                    .unwrap_or(&row.label)
            } else {
                &row.label
            };
            let (row_rect, row_response) = ui.allocate_exact_size(
                Vec2::new(content_width, TOC_ROW_HEIGHT),
                egui::Sense::click(),
            );
            let mut row_response = row_response.on_hover_cursor(egui::CursorIcon::PointingHand);
            let row_fill = if selected {
                palette().accent_soft
            } else if keyboard_focused || row_response.hovered() {
                ui.visuals().widgets.hovered.weak_bg_fill
            } else {
                Color32::TRANSPARENT
            };
            if row_fill != Color32::TRANSPARENT {
                ui.painter().rect_filled(row_rect, 6.0, row_fill);
            }
            if keyboard_focused {
                ui.painter().rect_stroke(
                    row_rect,
                    6.0,
                    egui::Stroke::new(1.0, palette().accent.gamma_multiply(0.72)),
                    egui::StrokeKind::Inside,
                );
            }
            let depth = u16::try_from(row.depth).unwrap_or(u16::MAX);
            let toggle_rect = Rect::from_min_size(
                Pos2::new(
                    row_rect.left() + 2.0 + f32::from(depth) * 12.0,
                    row_rect.top() + 5.0,
                ),
                Vec2::splat(26.0),
            );
            let toggle = if row.has_children {
                let expanded = self.ui.expanded_toc.contains(&row.id);
                toc_toggle_button(
                    ui,
                    toggle_rect.center(),
                    &row.id,
                    expanded,
                    selected || keyboard_focused,
                    self.language.text("折叠", "Collapse"),
                    self.language.text("展开", "Expand"),
                )
            } else {
                false
            };
            let label_rect = toc_label_rect(row_rect, toggle_rect);
            if paint_toc_label(ui, label_rect, display_label, selected || keyboard_focused) {
                row_response = row_response.on_hover_text(display_label);
            }
            row_response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), display_label)
            });
            let navigate = row_response.clicked() && !toggle;

            if toggle {
                self.toggle_toc(&row.id);
            }
            if navigate && let Some(target) = row.target {
                self.ui.toc_keyboard_row = Some(visible_index);
                self.go_to_toc(&row.id, &target);
                navigated_row = Some(visible_index);
            }
        }
        navigated_row
    }

    fn toc(&mut self, ui: &mut egui::Ui) {
        self.pdf_toc_controls(ui);
        let row_indices = self.visible_toc_row_indices();
        let active = self.snapshot.active_toc_id.clone();
        let should_auto_scroll = active != self.ui.last_auto_scrolled_toc;
        let active_row = active.as_ref().and_then(|active| {
            row_indices.iter().position(|&index| {
                self.reader.toc_items().get(index).map(|row| &row.id) == Some(active)
            })
        });
        let keyboard_row = self
            .ui
            .toc_keyboard_row
            .filter(|&row| row < row_indices.len());
        self.ui.toc_keyboard_row = keyboard_row;
        let should_auto_scroll_keyboard =
            keyboard_row != self.ui.last_auto_scrolled_toc_keyboard_row;
        let item_spacing = ui.spacing().item_spacing.y;
        let row_stride = TOC_ROW_HEIGHT + item_spacing;
        let content_height = toc_content_height(row_indices.len(), item_spacing);
        let scroll_area = egui::ScrollArea::vertical()
            .id_salt(TOC_SCROLL_ID_SALT)
            .auto_shrink([false, false]);
        let keyboard_scroll_delta = if self.is_focus_mode() && self.ui.sidebar_open {
            ui.input(|input| {
                if input.key_pressed(egui::Key::PageUp) {
                    ui.available_height() * 0.8
                } else if input.key_pressed(egui::Key::PageDown) {
                    -ui.available_height() * 0.8
                } else {
                    0.0
                }
            })
        } else {
            0.0
        };
        let mut preserve_bottom_after_navigation = false;
        scroll_area.show_viewport(ui, |ui, viewport| {
            ui.set_height(content_height);

            if keyboard_scroll_delta.abs() > f32::EPSILON {
                ui.scroll_with_delta(Vec2::new(0.0, keyboard_scroll_delta));
            }

            let auto_scroll_row = if should_auto_scroll_keyboard {
                keyboard_row
            } else if should_auto_scroll {
                active_row
            } else {
                None
            };
            if let Some(auto_scroll_row) = auto_scroll_row {
                let row_top = ui.max_rect().top() + toc_row_top(auto_scroll_row, item_spacing);
                let row_rect = Rect::from_min_size(
                    Pos2::new(ui.max_rect().left(), row_top),
                    Vec2::new(ui.max_rect().width(), TOC_ROW_HEIGHT),
                );
                ui.scroll_to_rect(row_rect, Some(egui::Align::Center));
            }

            let visible_rows = stable_virtual_row_range(viewport, row_stride, row_indices.len());
            let y_min = ui.max_rect().top() + toc_row_top(visible_rows.start, item_spacing);
            let y_max = ui.max_rect().top() + toc_row_top(visible_rows.end, item_spacing);
            let visible_rect = Rect::from_x_y_ranges(ui.max_rect().x_range(), y_min..=y_max);
            ui.scope_builder(egui::UiBuilder::new().max_rect(visible_rect), |ui| {
                ui.skip_ahead_auto_ids(visible_rows.start);
                let navigated_row = self.paint_visible_toc_rows(
                    ui,
                    visible_rows,
                    &row_indices,
                    active.as_ref(),
                    keyboard_row,
                );
                preserve_bottom_after_navigation = navigated_row.is_some_and(|row_index| {
                    toc_navigation_keeps_bottom_offset(
                        viewport,
                        content_height,
                        row_index,
                        item_spacing,
                    )
                });
            });
        });
        self.ui.last_auto_scrolled_toc_keyboard_row = self.ui.toc_keyboard_row;
        update_toc_scroll_marker(
            &mut self.ui.last_auto_scrolled_toc,
            preserve_bottom_after_navigation,
            self.snapshot.active_toc_id.as_deref(),
            active,
            should_auto_scroll,
            active_row.is_some(),
        );
    }

    fn pdf_toc_controls(&mut self, ui: &mut egui::Ui) {
        if self.format != rebook_formats::BookFormat::Pdf
            || self.source.table_of_contents_origin()
                == rebook_publication::TableOfContentsOrigin::Embedded
            || !self.plugin_settings.ocr_enabled
        {
            return;
        }
        let pending = self.pdf_toc.task.is_pending();
        let generated = self.source.table_of_contents_origin()
            == rebook_publication::TableOfContentsOrigin::Generated;
        let recognize_label = if generated {
            self.language.text("重新识别目录", "Regenerate contents")
        } else {
            self.language
                .text("AI 识别目录", "Generate contents with AI")
        };
        ui.horizontal(|ui| {
            let recognize = ui.add_enabled_ui(!pending, |ui| small_icon_button(ui, Icon::ScanText));
            let recognize = if pending {
                recognize
                    .inner
                    .on_disabled_hover_text(self.pdf_toc.progress.as_str())
            } else {
                recognize.inner.on_hover_text(recognize_label)
            };
            if recognize.clicked() {
                self.start_pdf_toc_generation();
            }
            if generated
                && small_icon_button(ui, Icon::Pencil)
                    .on_hover_text(self.language.text("编辑目录", "Edit contents"))
                    .clicked()
            {
                self.edit_generated_toc();
            }
        });
    }

    fn pdf_toc_review(&mut self, ctx: &egui::Context) {
        if !self.pdf_toc.editing {
            return;
        }
        let Some(draft) = self.pdf_toc.draft.as_mut() else {
            return;
        };
        let mut apply = false;
        let mut cancel = false;
        let mut remove = None;
        let modal = egui::Modal::new(egui::Id::new("pdf-toc-review-modal"))
            .area(egui::Modal::default_area(egui::Id::new(
                "pdf-toc-review-modal",
            )))
            .backdrop_color(Color32::BLACK.gamma_multiply(0.42))
            .frame(
                egui::Frame::new()
                    .fill(palette().surface)
                    .stroke(egui::Stroke::new(1.0, palette().border))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::symmetric(22, 18)),
            )
            .show(ctx, |ui| {
                let width = 600.0_f32.min((ctx.content_rect().width() - 32.0).max(320.0));
                ui.set_width(width);
                ui.heading(
                    self.language
                        .text("编辑 AI 目录", "Edit generated contents"),
                );
                let entry_count = match self.language.resolved() {
                    crate::preferences::AppLanguage::SimplifiedChinese => {
                        format!("{} 个条目", draft.entries.len())
                    }
                    crate::preferences::AppLanguage::English => {
                        format!("{} entries", draft.entries.len())
                    }
                    crate::preferences::AppLanguage::System => unreachable!(),
                };
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {}",
                        draft.provider_name, draft.model, entry_count
                    ))
                    .color(palette().muted),
                );
                ui.add_space(10.0);
                remove = pdf_toc_editor_table(
                    ui,
                    draft,
                    self.language,
                    self.source.book().sections.len(),
                    (ctx.content_rect().height() - 210.0).clamp(220.0, 520.0),
                );
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if dialog_action_button(ui, self.language.text("保存", "Save"), true).clicked()
                    {
                        apply = true;
                    }
                    if dialog_action_button(ui, self.language.text("取消", "Cancel"), false)
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });
        if let Some(index) = remove {
            draft.entries.remove(index);
        }
        if modal.should_close() {
            cancel = true;
        }
        if apply {
            match self.apply_generated_toc() {
                Ok(()) => {
                    self.reopen_notice = Some(
                        self.language
                            .text("PDF 目录已更新", "PDF contents updated")
                            .into(),
                    );
                    self.reopen_error = None;
                }
                Err(error) => self.show_error(error),
            }
        } else if cancel {
            self.pdf_toc.editing = false;
            self.pdf_toc.draft = None;
        }
    }

    fn highlights(&mut self, ui: &mut egui::Ui) {
        let highlights = self.highlights.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let content_width = (ui.available_width() - 12.0).max(1.0);
                ui.set_width(content_width);
                for highlight in highlights {
                    let selected = self.selected_highlight_id.as_deref() == Some(&highlight.id);
                    let label = highlight.note.as_ref().map_or_else(
                        || highlight.quote.clone(),
                        |note| format!("{}\n{}", highlight.quote, note),
                    );
                    let row_height = if highlight.note.is_some() {
                        TOOLBAR_CONTROL_SIZE * 1.6
                    } else {
                        TOOLBAR_CONTROL_SIZE
                    };
                    ui.horizontal(|ui| {
                        ui.set_width(content_width);
                        let quote_width = (ui.available_width()
                            - TOOLBAR_CONTROL_SIZE
                            - ui.spacing().item_spacing.x)
                            .max(1.0);
                        let quote_response = ui
                            .add_sized(
                                [quote_width, row_height],
                                egui::Button::selectable(
                                    selected,
                                    RichText::new(&label).size(crate::ui::scaled_font_size(12.0)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&label);
                        if quote_response.clicked() {
                            self.go_to_highlight(&highlight.id);
                        }
                        if icon_button(ui, Icon::Trash2)
                            .on_hover_text(self.language.text("删除", "Delete"))
                            .clicked()
                        {
                            self.remove_highlight(&highlight.id);
                        }
                    });
                    ui.separator();
                }
            });
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the search panel keeps mode, query, status, and result interactions together"
    )]
    fn search(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let response = compact_input_frame()
            .show(ui, |ui| {
                ui.set_min_width((width - 16.0).max(1.0));
                ui.horizontal(|ui| {
                    let input_width = ui.available_width().max(48.0);
                    let response = ui.add_sized(
                        [input_width, 32.0],
                        egui::TextEdit::singleline(&mut self.search.query)
                            .hint_text(self.language.text("搜索正文", "Search book"))
                            .frame(egui::Frame::NONE)
                            .vertical_align(egui::Align::Center)
                            .margin(egui::Margin::symmetric(2, 0)),
                    );
                    response
                })
                .inner
            })
            .inner;
        if std::mem::take(&mut self.search.focus_input) {
            response.request_focus();
        }
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            self.start_search();
        }
        if !self.search.status.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new(&self.search.status)
                    .size(crate::ui::scaled_font_size(12.0))
                    .color(palette().muted),
            );
        }
        let results = self.search.results.clone();
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for result in results {
                    if search_result_card(ui, &result, self.language).clicked() {
                        self.go_to_search_result(&result);
                    }
                    ui.add_space(6.0);
                }
            });
    }

    fn assistant(&mut self, ui: &mut egui::Ui) {
        self.assistant_header(ui);

        let busy = self.chat.task.is_pending();
        let reference_rows =
            u16::try_from(self.chat.references.len().div_ceil(2)).unwrap_or(u16::MAX);
        let reference_height = f32::from(reference_rows) * 28.0;
        let error_height = if self.chat.error.is_some() { 54.0 } else { 0.0 };
        let confirmation_height = if self.chat.pending_annotation_actions.is_empty() {
            0.0
        } else {
            78.0 + 18.0
                * f32::from(
                    u16::try_from(self.chat.pending_annotation_actions.len().min(3)).unwrap_or(3),
                )
        };
        let conversation_height = (ui.available_height()
            - ASSISTANT_COMPOSER_RESERVED_HEIGHT
            - ASSISTANT_BOTTOM_PADDING
            - reference_height
            - error_height
            - confirmation_height)
            .max(96.0);
        self.assistant_conversation(ui, conversation_height, busy);
        self.assistant_error(ui);
        self.assistant_annotation_confirmation(ui);
        self.assistant_composer(ui);
    }

    fn assistant_header(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let header = ui.allocate_ui_with_layout(
            Vec2::new(width, TOOLBAR_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(icon(Icon::MessageCircle).color(palette().muted));
                ui.label(
                    RichText::new(self.language.text("对话", "Chat"))
                        .size(crate::ui::scaled_font_size(14.0))
                        .strong()
                        .color(palette().text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, Icon::X)
                        .on_hover_text(self.language.text("关闭", "Close"))
                        .clicked()
                    {
                        self.close_assistant_panel();
                    }
                    ui.add_enabled_ui(!self.chat.messages.is_empty(), |ui| {
                        if icon_button(ui, Icon::Trash2)
                            .on_hover_text(self.language.text("清空", "Clear"))
                            .clicked()
                        {
                            self.clear_chat();
                        }
                    });
                });
            },
        );
        ui.painter().hline(
            header.response.rect.left()..=header.response.rect.right(),
            header.response.rect.bottom(),
            egui::Stroke::new(1.0, palette().border),
        );
        ui.add_space(10.0);
    }

    fn assistant_conversation(&mut self, ui: &mut egui::Ui, height: f32, busy: bool) {
        let messages = self.chat.messages.clone();
        let streaming_content = self
            .chat
            .streaming
            .as_ref()
            .map(|streaming| streaming.content.clone());
        let routed_scroll = self.assistant_conversation_scroll_delta(ui);
        let mut clicked_citation = None;
        let scroll_output = egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if routed_scroll != 0.0 {
                    ui.scroll_with_delta(Vec2::new(0.0, routed_scroll));
                }
                let content_width =
                    (ui.available_width() - ASSISTANT_SCROLLBAR_GUTTER).max(1.0);
                ui.set_width(content_width);
                if messages.is_empty() && !busy {
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), height),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            ui.add_space(ASSISTANT_EMPTY_TOP_PADDING);
                            ui.add(icon(Icon::MessageCircle).size(27.0).color(palette().muted));
                            ui.label(
                                RichText::new(
                                    self.language
                                        .text("围绕当前书籍提问", "Ask about this book"),
                                )
                                .strong()
                                .color(palette().text),
                            );
                            ui.label(
                                RichText::new(self.language.text(
                                    "可以总结章节、解释选中的段落，\n或搜索书中的概念。",
                                    "Summarize sections, explain a selection,\nor find concepts in the book.",
                                ))
                                .size(crate::ui::scaled_font_size(12.0))
                                .color(palette().muted),
                            );
                        },
                    );
                }
                for (message_ordinal, message) in messages.iter().enumerate() {
                    capture_clicked_citation(
                        &mut clicked_citation,
                        chat_message_card(
                            ui,
                            message.role,
                            message
                                .display_content
                                .as_deref()
                                .unwrap_or(&message.content),
                            self.language,
                            &mut self.chat_markdown,
                            message_ordinal,
                            false,
                        ),
                    );
                    ui.add_space(10.0);
                }
                if busy {
                    let content = streaming_content
                        .as_deref()
                        .filter(|content| !content.is_empty())
                        .unwrap_or_else(|| {
                            self.language.text(
                                "正在阅读和检索书籍…",
                                "Reading and searching the book…",
                            )
                        });
                    capture_clicked_citation(
                        &mut clicked_citation,
                        chat_message_card(
                            ui,
                            ChatRole::Assistant,
                            content,
                            self.language,
                            &mut self.chat_markdown,
                            messages.len(),
                            true,
                        ),
                    );
                }
            });
        let clicked_visual_preview = self.chat_markdown.take_clicked_visual_preview();
        auto_scroll_assistant_selection(ui.ctx(), &scroll_output);
        if let Some(locator) = clicked_citation {
            self.open_chat_citation(&locator);
        }
        if let Some(preview) = clicked_visual_preview {
            self.open_color_image_preview(ui.ctx(), preview.image, "chat-visual-preview");
        }
    }

    fn assistant_conversation_scroll_delta(&mut self, ui: &mut egui::Ui) -> f32 {
        if self.is_focus_mode() && self.ui.assistant_panel.is_some() {
            let keyboard = std::mem::take(&mut self.chat.pending_keyboard_scroll_delta);
            let wheel = ui.input_mut(|input| {
                let delta = input.smooth_scroll_delta.y;
                input.smooth_scroll_delta.y = 0.0;
                delta
            });
            return wheel - keyboard;
        }
        -assistant_keyboard_scroll_input(
            ui,
            &self.chat.input,
            self.chat.cursor_char_index,
            &self.chat.references,
        )
        .unwrap_or_default()
    }

    fn assistant_error(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.chat.error {
            egui::Frame::new()
                .fill(palette().error_fill)
                .stroke(egui::Stroke::new(1.0, palette().error_stroke))
                .corner_radius(8)
                .inner_margin(9)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(error)
                            .size(crate::ui::scaled_font_size(12.0))
                            .color(palette().error_text),
                    );
                });
        }
    }

    fn assistant_annotation_confirmation(&mut self, ui: &mut egui::Ui) {
        let count = self.chat.pending_annotation_actions.len();
        if count == 0 {
            return;
        }
        let mut confirm = false;
        let mut cancel = false;
        egui::Frame::new()
            .fill(palette().surface)
            .stroke(egui::Stroke::new(1.0, palette().border))
            .corner_radius(8)
            .inner_margin(9)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(match self.language.resolved() {
                        crate::preferences::AppLanguage::SimplifiedChinese => {
                            format!("AI 请求执行 {count} 项批注操作")
                        }
                        crate::preferences::AppLanguage::English => {
                            format!("AI requested {count} annotation action(s)")
                        }
                        crate::preferences::AppLanguage::System => unreachable!(),
                    })
                    .size(crate::ui::scaled_font_size(12.0))
                    .color(palette().text),
                );
                for action in self.chat.pending_annotation_actions.iter().take(3) {
                    let detail = match action {
                        crate::plugins::ChatAnnotationAction::Create(annotation) => format!(
                            "{}: {}",
                            self.language.text("新增", "Create"),
                            clipped_annotation_action_text(&annotation.quote)
                        ),
                        crate::plugins::ChatAnnotationAction::Update(annotation) => format!(
                            "{}: {}",
                            self.language.text("修改", "Update"),
                            clipped_annotation_action_text(
                                annotation.note.as_deref().unwrap_or("（清空批注）")
                            )
                        ),
                        crate::plugins::ChatAnnotationAction::Delete { annotation_id } => {
                            format!(
                                "{}: {}",
                                self.language.text("删除", "Delete"),
                                clipped_annotation_action_text(annotation_id)
                            )
                        }
                    };
                    ui.label(
                        RichText::new(detail)
                            .size(crate::ui::scaled_font_size(11.0))
                            .color(palette().muted),
                    );
                }
                ui.horizontal(|ui| {
                    confirm = ui.button(self.language.text("确认", "Confirm")).clicked();
                    cancel = ui.button(self.language.text("取消", "Cancel")).clicked();
                });
            });
        if confirm {
            self.confirm_chat_annotation_actions();
        } else if cancel {
            self.cancel_chat_annotation_actions();
        }
    }

    fn assistant_composer(&mut self, ui: &mut egui::Ui) {
        self.assistant_composer_with_options(ui, true, true, false);
    }

    fn assistant_composer_with_options(
        &mut self,
        ui: &mut egui::Ui,
        show_reference_chips: bool,
        show_container: bool,
        compact: bool,
    ) {
        if !compact {
            ui.add_space(6.0);
        }
        let busy = self.chat.task.is_pending();
        let input_id = ui.make_persistent_id("assistant-chat-input");
        let (initial_references, initial_commands) = self.assistant_suggestions(busy);
        let keys = assistant_composer_keys(
            ui,
            input_id,
            active_suggestion_count(&initial_references, &initial_commands),
        );
        let render =
            self.assistant_composer_input(ui, input_id, show_reference_chips, show_container);
        let (reference_suggestions, command_suggestions) = self.assistant_suggestions(busy);
        let suggestion_count =
            active_suggestion_count(&reference_suggestions, &command_suggestions);
        self.chat.suggestion_index = self
            .chat
            .suggestion_index
            .min(suggestion_count.saturating_sub(1));

        let mut submit = render.submit;
        let suggestion_applied = self.apply_assistant_suggestion_key(
            keys,
            &reference_suggestions,
            &command_suggestions,
            &render.input_response,
            &mut submit,
        );
        if !suggestion_applied && suggestion_count > 0 {
            let (picked_reference, picked_command, hovered_index) = assistant_suggestion_popup(
                ui,
                render.composer_rect,
                &reference_suggestions,
                &command_suggestions,
                self.chat.suggestion_index,
                self.language,
            );
            if let Some(index) = hovered_index {
                self.chat.suggestion_index = index;
            }
            if let Some(reference) = picked_reference {
                self.select_chat_reference(reference);
                render.input_response.request_focus();
            } else if let Some(command) = picked_command {
                self.select_chat_command(command);
                render.input_response.request_focus();
            }
        }
        if submit {
            self.send_chat();
        }
        if !compact {
            ui.add_space(ASSISTANT_BOTTOM_PADDING);
        }
    }

    fn assistant_suggestions(&mut self, busy: bool) -> (Vec<ChatReference>, Vec<ChatCommand>) {
        let reference_token_active = chat_reference_token(
            &self.chat.input,
            self.chat.cursor_char_index,
            &self.chat.references,
        )
        .is_some();
        let references = if busy {
            Vec::new()
        } else {
            self.current_chat_reference_suggestions()
        };
        let commands = if busy || reference_token_active {
            Vec::new()
        } else {
            chat_command_suggestions(&self.chat.input)
        };
        (references, commands)
    }

    fn assistant_composer_input(
        &mut self,
        ui: &mut egui::Ui,
        input_id: egui::Id,
        show_reference_chips: bool,
        show_container: bool,
    ) -> AssistantComposerRender {
        let references = self.chat.references.clone();
        let mut remove_reference = None;
        let mut input_response = None;
        let mut submit = false;
        let move_cursor_to_end = std::mem::take(&mut self.chat.move_cursor_to_end);
        let frame = if show_container {
            compact_input_frame()
        } else {
            egui::Frame::NONE
        };
        let composer = frame.show(ui, |ui| {
            if show_reference_chips {
                remove_reference = chat_reference_chips(ui, &references, self.language);
            }
            ui.horizontal(|ui| {
                let input_width = (ui.available_width() - 38.0).max(48.0);
                let hint_text = self.language.text(
                    "询问这本书，输入 / 使用技能或 @ 引用…",
                    "Ask this book, type / for skills or @ to reference…",
                );
                let (mut output, _) = centered_assistant_text_edit(
                    ui,
                    &mut self.chat.input,
                    input_id,
                    input_width,
                    hint_text,
                );
                if output.response.changed() {
                    self.chat.suggestion_index = 0;
                }
                self.chat.cursor_char_index = output.cursor_range.map_or_else(
                    || self.chat.input.chars().count(),
                    |range| range.primary.index.into(),
                );
                if move_cursor_to_end {
                    let cursor = CCursor::new(self.chat.input.chars().count());
                    output
                        .state
                        .cursor
                        .set_char_range(Some(CCursorRange::one(cursor)));
                    output.state.store(ui.ctx(), output.response.id);
                    output.response.request_focus();
                    self.chat.cursor_char_index = cursor.index.into();
                }
                input_response = Some(output.response.response.clone());
                submit = icon_button(ui, Icon::Send)
                    .on_hover_text(self.language.text("发送", "Send"))
                    .clicked();
            });
        });
        if let Some(id) = remove_reference {
            self.remove_chat_reference(&id);
        }
        AssistantComposerRender {
            composer_rect: composer.response.rect,
            input_response: input_response.expect("chat input is always rendered"),
            submit,
        }
    }

    fn apply_assistant_suggestion_key(
        &mut self,
        keys: AssistantComposerKeys,
        references: &[ChatReference],
        commands: &[ChatCommand],
        input_response: &egui::Response,
        submit: &mut bool,
    ) -> bool {
        let suggestion_count = active_suggestion_count(references, commands);
        if keys.input_had_focus && keys.initial_suggestion_count > 0 && suggestion_count > 0 {
            match keys.movement {
                AssistantSuggestionMovement::Forward => {
                    self.chat.suggestion_index =
                        move_suggestion_index(self.chat.suggestion_index, suggestion_count, true);
                }
                AssistantSuggestionMovement::Backward => {
                    self.chat.suggestion_index =
                        move_suggestion_index(self.chat.suggestion_index, suggestion_count, false);
                }
                AssistantSuggestionMovement::None => {}
            }
        }
        let input_is_active = input_response.has_focus() || input_response.lost_focus();
        let apply = input_is_active
            && keys.initial_suggestion_count > 0
            && suggestion_count > 0
            && keys.acceptance != AssistantSuggestionAcceptance::None;
        if !apply {
            if input_is_active
                && keys.acceptance == AssistantSuggestionAcceptance::Enter
                && suggestion_count == 0
            {
                *submit = true;
            }
            return false;
        }
        if let Some(reference) = references.get(self.chat.suggestion_index).cloned() {
            self.select_chat_reference(reference);
            input_response.request_focus();
            return true;
        }
        let Some(command) = commands.get(self.chat.suggestion_index).copied() else {
            return false;
        };
        let exact_non_argument_command =
            !command.requires_args && self.chat.input.trim().eq_ignore_ascii_case(command.name);
        if keys.acceptance == AssistantSuggestionAcceptance::Enter && exact_non_argument_command {
            *submit = true;
        } else {
            self.select_chat_command(command);
            input_response.request_focus();
        }
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the compact reader menu keeps its mutually exclusive mode actions together"
    )]
    fn menu(&mut self, ctx: &egui::Context) {
        let progress = self.ui.menu_motion.value.clamp(0.0, 1.0);
        if progress <= 0.001 {
            return;
        }
        let assistant_inset = if self.is_focus_mode() {
            0.0
        } else {
            ASSISTANT_WIDTH * self.ui.assistant_motion.value.clamp(0.0, 1.0)
        };
        let menu = egui::Area::new("reader-menu".into())
            .order(egui::Order::Tooltip)
            .anchor(
                egui::Align2::RIGHT_TOP,
                Vec2::new(
                    -12.0 - assistant_inset,
                    TOOLBAR_HEIGHT + 8.0 - (1.0 - progress) * 8.0,
                ),
            )
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(palette().surface)
                    .corner_radius(9)
                    .inner_margin(6)
                    .show(ui, |ui| {
                        ui.set_width(180.0);
                        if navigation_button(
                            ui,
                            Icon::Settings,
                            self.language.text("设置", "Settings"),
                            false,
                        )
                        .clicked()
                        {
                            self.request_settings();
                        }
                        let focus_allowed = self.focus_mode_allowed() || self.is_focus_mode();
                        let mode_button = ui.add_enabled_ui(focus_allowed, |ui| {
                            navigation_button(
                                ui,
                                Icon::BrainCircuit,
                                if self.is_focus_mode() {
                                    self.language.text("专注模式", "Focus mode")
                                } else {
                                    self.language.text("经典模式", "Classic mode")
                                },
                                false,
                            )
                        });
                        let mode_button = if focus_allowed {
                            mode_button.inner
                        } else {
                            mode_button.inner.on_disabled_hover_text(self.language.text(
                                "原始 PDF 不支持专注模式，请先切换到 OCR 版式",
                                "Original PDF does not support Focus mode; switch to OCR reflow first",
                            ))
                        };
                        if mode_button.clicked()
                        {
                            let mode = if self.is_focus_mode() {
                                crate::preferences::ReadingMode::Classic
                            } else {
                                crate::preferences::ReadingMode::Focus
                            };
                            self.request_settings_change(ReaderSettingsChange::ReadingMode(mode));
                        }
                        if !self.is_focus_mode()
                            && navigation_button(
                                ui,
                                Icon::BookOpen,
                                match self.reader.style().spread {
                                    SpreadMode::Single => {
                                        self.language.text("单栏模式", "Single mode")
                                    }
                                    SpreadMode::Double => {
                                        self.language.text("双栏模式", "Double mode")
                                    }
                                    SpreadMode::Scroll => {
                                        self.language.text("滑动模式", "Scroll mode")
                                    }
                                },
                                false,
                            )
                            .clicked()
                        {
                            let spread = if self.is_scroll_mode() {
                                SpreadMode::Double
                            } else {
                                SpreadMode::Scroll
                            };
                            self.request_settings_change(ReaderSettingsChange::Spread(spread));
                        }
                        let theme = crate::ui::theme_preference();
                        if navigation_button(
                            ui,
                            match theme {
                                AppTheme::System => Icon::Monitor,
                                AppTheme::Light => Icon::Sun,
                                AppTheme::Dark => Icon::Moon,
                            },
                            match theme {
                                AppTheme::System => {
                                    self.language.text("跟随系统", "Follow system")
                                }
                                AppTheme::Light => {
                                    self.language.text("浅色模式", "Light mode")
                                }
                                AppTheme::Dark => {
                                    self.language.text("黑夜模式", "Dark mode")
                                }
                            },
                            false,
                        )
                        .clicked()
                        {
                            self.request_settings_change(ReaderSettingsChange::Theme(theme.next()));
                        }
                        if !self.is_focus_mode() {
                            self.selection_mode_menu(ui);
                        }
                    });
            });
        let clicked_outside = self.ui.overlay == ReaderOverlay::Menu
            && !self.ui.menu_motion.is_animating()
            && ctx.input(|input| {
                input.pointer.any_click()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|position| !menu.response.rect.contains(position))
            });
        if clicked_outside {
            self.close_overlay();
        }
    }

    fn selection_mode_menu(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.label(
            RichText::new(self.language.text("文字选择", "Text selection"))
                .size(crate::ui::scaled_font_size(12.0))
                .color(palette().muted),
        );
        let choices = [
            (
                SelectionGranularity::Free,
                self.language.text("自由", "Free"),
            ),
            (
                SelectionGranularity::Word,
                self.language.text("单词", "Word"),
            ),
            (
                SelectionGranularity::Sentence,
                self.language.text("句子", "Sentence"),
            ),
            (
                SelectionGranularity::Paragraph,
                self.language.text("段落", "Paragraph"),
            ),
        ];
        let requested = choices.into_iter().find_map(|(granularity, label)| {
            navigation_text_button(ui, label, self.selection_granularity == granularity)
                .clicked()
                .then_some(granularity)
        });
        if let Some(granularity) = requested {
            self.selection_granularity = granularity;
            self.request_settings_change(ReaderSettingsChange::SelectionGranularity(granularity));
        }
    }

    fn selection_action_anchor(
        &mut self,
        rects: &[rebook_reader::ReaderSelectionRect],
    ) -> Option<rebook_reader::ReaderSelectionRect> {
        if self.is_scroll_mode() {
            return rects.last().copied();
        }
        match self.reader.current_spread_positions() {
            Ok(positions) => last_visible_selection_rect(rects, &positions),
            Err(error) => {
                self.error = Some(format!(
                    "Resolve selection toolbar position failed: {error}"
                ));
                rects.last().copied()
            }
        }
    }

    fn selection_actions(&mut self, ctx: &egui::Context, page_rect: Rect) {
        if !self.selection_toolbar_visible {
            return;
        }
        let Some(selection) = &self.selection else {
            return;
        };
        let selection_text = selection.text.clone();
        let selection_rects = selection.rects.clone();
        let anchor = self.selection_action_anchor(&selection_rects);
        let position = anchor.map_or(page_rect.center(), |rect| {
            let page_top = if self.is_scroll_mode() {
                self.scroll_section
                    .as_ref()
                    .and_then(|layout| layout.content_y_for_position(rect.position, rect.y))
                    .zip(self.scroll_viewport)
                    .map_or(0.0, |(content_y, viewport)| {
                        content_y - rect.y + self.scroll_content_padding(viewport.size.y)
                            - viewport.offset_y
                    })
            } else {
                0.0
            };
            Pos2::new(
                page_rect.left() + rect.x + rect.width * 0.5,
                page_rect.top() + page_top + rect.y + rect.height + 8.0,
            )
        });
        let mut copy_selection = false;
        let mut create_highlight = false;
        let mut open_note = false;
        let mut save_note = false;
        let mut cancel_note = false;
        let mut explain = false;
        egui::Area::new("selection-actions".into())
            .order(egui::Order::Tooltip)
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(position)
            .constrain(true)
            .show(ctx, |ui| {
                let note_open = self.annotation_note_draft.is_some();
                selection_popover_frame(if note_open { 12 } else { 4 }).show(ui, |ui| {
                    if let Some(draft) = self.annotation_note_draft.as_mut() {
                        match annotation_editor(ui, draft, self.language) {
                            AnnotationEditorAction::None => {}
                            AnnotationEditorAction::Save => save_note = true,
                            AnnotationEditorAction::Cancel => cancel_note = true,
                        }
                    } else {
                        ui.horizontal(|ui| {
                            copy_selection = icon_button(ui, Icon::Copy)
                                .on_hover_text(self.language.text("复制", "Copy"))
                                .clicked();
                            create_highlight = icon_button(ui, Icon::Highlighter)
                                .on_hover_text(self.language.text("高亮", "Highlight"))
                                .clicked();
                            open_note = icon_button(ui, Icon::MessageSquarePlus)
                                .on_hover_text(self.language.text("添加批注", "Add note"))
                                .clicked();
                            explain = icon_button(ui, Icon::MessageCircleQuestion)
                                .on_hover_text(self.language.text("解释", "Explain"))
                                .clicked();
                        });
                    }
                });
            });
        if copy_selection {
            ctx.copy_text(selection_text);
            self.notice_timer.show(
                &mut self.notice,
                self.language
                    .text("已复制到剪贴板", "Copied to clipboard")
                    .into(),
                Instant::now(),
            );
            ctx.request_repaint_after(super::NOTICE_AUTO_DISMISS_DELAY);
        } else if create_highlight {
            self.create_highlight(None);
        } else if open_note {
            self.annotation_note_draft = Some(AnnotationDraft {
                focus_pending: true,
                ..AnnotationDraft::default()
            });
        } else if save_note {
            let note = self
                .annotation_note_draft
                .as_ref()
                .map(|draft| draft.note.clone());
            self.create_highlight(note);
        } else if cancel_note {
            self.annotation_note_draft = None;
        } else if explain {
            self.explain_selection();
        }
    }

    fn classic_footnote_hover_interaction(&mut self, response: &egui::Response) {
        if self.is_focus_mode() {
            return;
        }
        let hover_position = response.ctx.input(|input| input.pointer.hover_pos());
        let over_overlay = hover_position.is_some_and(|position| {
            self.classic_footnote_overlay_rect
                .is_some_and(|rect| rect.expand(12.0).contains(position))
        });
        if let Some(position) = hover_position.filter(|position| response.rect.contains(*position))
        {
            let x = position.x - response.rect.min.x;
            let y = position.y - response.rect.min.y;
            match self.classic_footnotes_at_canvas(x, y) {
                Ok(Some(footnotes)) => {
                    self.classic_footnotes = footnotes;
                    self.classic_footnote_anchor_y = Some(position.y);
                    self.ui.focus_footnotes_visible = true;
                    self.ui.focus_footnote_scroll_delta = 0.0;
                    response.ctx.request_repaint();
                    return;
                }
                Ok(None) => {}
                Err(error) => self.error = Some(format!("Footnote hover failed: {error}")),
            }
        }
        if !over_overlay {
            self.close_focus_footnotes();
        }
    }

    fn pointer_interaction(&mut self, response: &egui::Response) {
        self.classic_footnote_hover_interaction(response);
        let Some(position) = response.interact_pointer_pos() else {
            if !response.ctx.input(|input| input.pointer.primary_down()) {
                self.image_pointer_state = ImagePointerState::Idle;
            }
            return;
        };
        let x = position.x - response.rect.min.x;
        let y = position.y - response.rect.min.y;
        self.image_long_press_interaction(response, x, y);
        if !self.is_focus_mode() && response.drag_started() {
            self.begin_text_selection(x, y);
        }
        if !self.is_focus_mode() && response.dragged() {
            self.update_text_selection(x, y);
        }
        if !self.is_focus_mode() && response.drag_stopped() {
            // `drag_delta()` is zero on egui's release frame. A
            // `drag_stopped` response has already crossed the drag threshold,
            // so treating it as moved preserves the completed selection.
            self.finish_text_selection(x, y, true);
        }
        if response.clicked() {
            if matches!(
                self.image_pointer_state,
                ImagePointerState::SuppressNextClick
            ) {
                self.image_pointer_state = ImagePointerState::Idle;
                return;
            }
            if self.try_open_image_preview(&response.ctx, x, y) {
                return;
            }
            if self.is_focus_mode() {
                self.cancel_text_selection();
                self.focus_clicked_unit(x, y);
                return;
            }
            self.finish_text_selection(x, y, false);
            if self.selected_highlight_id.is_none() {
                self.focus_clicked_unit(x, y);
            }
        }
    }

    fn image_long_press_interaction(&mut self, response: &egui::Response, x: f32, y: f32) {
        let ctx = &response.ctx;
        let (primary_pressed, primary_down) = ctx.input(|input| {
            (
                input.pointer.primary_pressed(),
                input.pointer.primary_down(),
            )
        });
        let pointer = Pos2::new(x, y);
        if primary_pressed {
            self.selected_image = None;
            self.image_pointer_state = ImagePointerState::Idle;
            match self.image_at_canvas(x, y) {
                Ok(Some(image)) => {
                    self.image_pointer_state = ImagePointerState::Press(ImagePressCandidate {
                        started_at: Instant::now(),
                        origin: pointer,
                        image,
                        scroll_mode: self.is_scroll_mode(),
                    });
                }
                Ok(None) => {}
                Err(error) => self.error = Some(format!("Select image failed: {error}")),
            }
        }

        let now = Instant::now();
        let ready = matches!(
            &self.image_pointer_state,
            ImagePointerState::Press(candidate)
                if candidate.origin.distance(pointer) <= IMAGE_LONG_PRESS_MAX_TRAVEL
                    && now.saturating_duration_since(candidate.started_at)
                        >= IMAGE_LONG_PRESS_DURATION
        );
        if ready {
            let ImagePointerState::Press(candidate) =
                std::mem::replace(&mut self.image_pointer_state, ImagePointerState::Idle)
            else {
                unreachable!("ready long-press state must contain a candidate");
            };
            match SelectedImage::from_reader_image(&candidate.image, candidate.scroll_mode) {
                Ok(image) => {
                    self.cancel_text_selection();
                    self.selected_image = Some(image);
                    self.image_pointer_state = ImagePointerState::SuppressNextClick;
                    ctx.request_repaint();
                }
                Err(error) => self.error = Some(error.into()),
            }
        } else if matches!(
            &self.image_pointer_state,
            ImagePointerState::Press(candidate)
                if !primary_down
                    || candidate.origin.distance(pointer) > IMAGE_LONG_PRESS_MAX_TRAVEL
        ) {
            self.image_pointer_state = ImagePointerState::Idle;
        } else if let ImagePointerState::Press(candidate) = &self.image_pointer_state {
            let elapsed = now.saturating_duration_since(candidate.started_at);
            ctx.request_repaint_after(IMAGE_LONG_PRESS_DURATION.saturating_sub(elapsed));
        }
        if !primary_down && !response.clicked() {
            self.image_pointer_state = ImagePointerState::Idle;
        }
    }

    fn image_at_canvas(
        &mut self,
        x: f32,
        y: f32,
    ) -> Result<Option<ReaderImage>, rebook_reader::ReaderError> {
        if self.is_scroll_mode() {
            let Some((position, page_x, page_y)) = self.scroll_page_coordinates(x, y) else {
                return Ok(None);
            };
            self.reader.image_at_page(position, page_x, page_y)
        } else {
            self.reader.image_at_current_spread(x, y)
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "decoded image dimensions are GPU-bounded and egui geometry uses f32"
    )]
    fn try_open_image_preview(&mut self, ctx: &egui::Context, x: f32, y: f32) -> bool {
        if !supports_image_preview(self.format, self.pdf_ocr.mode) {
            return false;
        }
        let image = match self.image_at_canvas(x, y) {
            Ok(Some(image)) => image,
            Ok(None) => return false,
            Err(error) => {
                self.error = Some(format!("打开图片预览失败：{error}"));
                return false;
            }
        };
        let (Ok(width), Ok(height)) = (usize::try_from(image.width), usize::try_from(image.height))
        else {
            self.error = Some("图片尺寸超出预览范围".into());
            return false;
        };
        let Some(byte_len) = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            self.error = Some("图片尺寸超出预览范围".into());
            return false;
        };
        if width == 0 || height == 0 || image.pixels.len() < byte_len {
            self.error = Some("图片数据不完整，无法预览".into());
            return false;
        }

        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([width, height], &image.pixels[..byte_len]);
        self.open_color_image_preview(ctx, color_image, "reader-image-preview");
        true
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "GPU-bounded image dimensions are converted to egui's f32 geometry"
    )]
    fn open_color_image_preview(
        &mut self,
        ctx: &egui::Context,
        color_image: egui::ColorImage,
        texture_namespace: &str,
    ) {
        let source_size = Vec2::new(color_image.size[0] as f32, color_image.size[1] as f32);
        let texture = ctx.load_texture(
            format!("{texture_namespace}-{}", self.scene_revision),
            color_image.clone(),
            egui::TextureOptions::LINEAR,
        );
        self.cancel_text_selection();
        self.close_focus_footnotes();
        self.selected_image = None;
        self.image_pointer_state = ImagePointerState::Idle;
        self.image_preview = Some(super::ImagePreview {
            texture,
            image: color_image,
            source_size,
            zoom: 1.0,
            pan: Vec2::ZERO,
        });
        ctx.request_repaint();
    }

    fn copy_shortcut(&mut self, ctx: &egui::Context, interaction_blocked: bool) {
        let text_edit_focused = ctx.text_edit_focused();
        let copy_image = self
            .image_preview
            .as_ref()
            .map(|preview| preview.image.clone())
            .or_else(|| {
                (!text_edit_focused)
                    .then(|| {
                        self.selected_image
                            .as_ref()
                            .map(|image| image.image.clone())
                    })
                    .flatten()
            });
        let selection_text =
            if copy_image.is_none() && self.selection_toolbar_visible && !text_edit_focused {
                self.selection
                    .as_ref()
                    .map(|selection| selection.text.clone())
                    .filter(|text| !text.is_empty())
            } else {
                None
            };
        let focus_text = if copy_image.is_none() && selection_text.is_none() {
            focus_unit_copy_text(
                self.focus_body_accepts_shortcuts(interaction_blocked),
                text_edit_focused,
                self.current_focus_unit_is_image(),
                self.focus_units
                    .get(self.focus_unit_index)
                    .map(|unit| unit.clipboard_text.as_str()),
            )
            .map(str::to_owned)
        } else {
            None
        };
        if copy_image.is_none() && selection_text.is_none() && focus_text.is_none() {
            return;
        }
        if !ctx.input_mut(|input| consume_copy_shortcut(input, &self.shortcuts.copy)) {
            return;
        }

        if let Some(image) = copy_image {
            ctx.copy_image(image);
            self.show_copy_notice(ctx, true);
        } else if let Some(text) = selection_text {
            ctx.copy_text(text);
            self.show_copy_notice(ctx, false);
        } else if let Some(text) = focus_text {
            ctx.copy_text(text);
            self.show_copy_notice(ctx, false);
        }
    }

    fn show_copy_notice(&mut self, ctx: &egui::Context, image: bool) {
        let message = if image {
            self.language
                .text("图片已复制到剪贴板", "Image copied to clipboard")
        } else {
            self.language.text("已复制到剪贴板", "Copied to clipboard")
        };
        self.notice_timer
            .show(&mut self.notice, message.into(), Instant::now());
        ctx.request_repaint_after(super::NOTICE_AUTO_DISMISS_DELAY);
    }

    fn selected_image_overlay(&self, ctx: &egui::Context, page_rect: Rect) {
        let Some(selected) = &self.selected_image else {
            return;
        };
        let bounds = if selected.scroll_mode {
            if !self.is_scroll_mode() {
                return;
            }
            let Some((page_top, viewport)) = self
                .scroll_section
                .as_ref()
                .and_then(|layout| layout.content_y_for_position(selected.position, 0.0))
                .zip(self.scroll_viewport)
            else {
                return;
            };
            selected.bounds.translate(Vec2::new(
                page_rect.left(),
                page_rect.top() + page_top + self.scroll_content_padding(viewport.size.y)
                    - viewport.offset_y,
            ))
        } else {
            if self.is_scroll_mode() {
                return;
            }
            selected.bounds.translate(page_rect.min.to_vec2())
        };
        if !bounds.intersects(page_rect) {
            return;
        }

        let stroke = palette().accent;
        let painter = ctx
            .layer_painter(selected_image_layer_id())
            .with_clip_rect(page_rect);
        painter.rect_stroke(
            bounds,
            2.0,
            egui::Stroke::new(2.0, stroke),
            egui::StrokeKind::Inside,
        );
    }

    fn image_preview_overlay(&mut self, ctx: &egui::Context) {
        let mut close = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let Some(preview) = self.image_preview.as_mut() else {
            return;
        };
        let screen = ctx.content_rect();
        let available = Vec2::new(
            (screen.width() - IMAGE_PREVIEW_MARGIN * 2.0).max(1.0),
            (screen.height() - IMAGE_PREVIEW_MARGIN * 2.0).max(1.0),
        );
        let fit_scale = (available.x / preview.source_size.x)
            .min(available.y / preview.source_size.y)
            .min(1.0);

        let wheel_delta = ctx.input(preview_wheel_delta);
        if wheel_delta != 0.0 {
            let old_zoom = preview.zoom;
            preview.zoom = zoom_from_wheel(preview.zoom, wheel_delta);
            let ratio = preview.zoom / old_zoom;
            let pointer = ctx
                .input(|input| input.pointer.hover_pos())
                .unwrap_or_else(|| screen.center());
            let current_center = screen.center() + preview.pan;
            preview.pan -= (pointer - current_center) * (ratio - 1.0);
            ctx.request_repaint();
        }

        let display_size = preview.source_size * fit_scale * preview.zoom;
        preview.pan = clamp_preview_pan(preview.pan, display_size, available);
        let image_rect = Rect::from_center_size(screen.center() + preview.pan, display_size);
        let texture_id = preview.texture.id();
        let zoom_percent = preview.zoom * 100.0;
        let interaction =
            show_image_preview_area(ctx, screen, image_rect, texture_id, zoom_percent);
        close |= interaction.close;

        if interaction.reset {
            preview.zoom = 1.0;
            preview.pan = Vec2::ZERO;
            ctx.request_repaint();
        } else if interaction.drag_delta != Vec2::ZERO {
            preview.pan = clamp_preview_pan(
                preview.pan + interaction.drag_delta,
                display_size,
                available,
            );
            ctx.request_repaint();
        }
        if close {
            self.image_preview = None;
            ctx.request_repaint();
        }
    }

    fn feedback(&mut self, ctx: &egui::Context) {
        if let Some(error) = self.translation.error.clone() {
            if show_toast(
                ctx,
                "translation-error",
                &error,
                ToastKind::Error,
                Vec2::new(-18.0, 62.0),
                true,
            ) {
                self.dismiss_translation_notice();
            }
            return;
        }
        if let Some(error) = &self.error {
            show_toast(
                ctx,
                "reader-error",
                error,
                ToastKind::Error,
                Vec2::new(-18.0, 62.0),
                false,
            );
        } else if let Some(notice) = &self.notice {
            show_toast(
                ctx,
                "reader-notice",
                notice,
                ToastKind::Success,
                Vec2::new(-18.0, 62.0),
                false,
            );
        }
    }
}

fn paint_toc_label(ui: &egui::Ui, rect: Rect, label: &str, selected: bool) -> bool {
    if rect.width() <= 0.0 {
        return false;
    }
    let color = if selected {
        palette().accent
    } else {
        palette().text
    };
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let painter = ui.painter();
    let (display_label, elided) = elide_text_to_width(label, rect.width(), |text| {
        painter
            .layout_no_wrap(text.to_owned(), font_id.clone(), color)
            .size()
            .x
    });
    let galley = painter.layout_no_wrap(display_label, font_id, color);
    painter.with_clip_rect(rect).galley(
        Pos2::new(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    elided
}

fn elide_text_to_width(
    label: &str,
    max_width: f32,
    mut measure: impl FnMut(&str) -> f32,
) -> (String, bool) {
    const ELLIPSIS: &str = "…";

    if measure(label) <= max_width {
        return (label.to_owned(), false);
    }
    if measure(ELLIPSIS) > max_width {
        return (String::new(), true);
    }

    let mut boundaries = label
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(label.len());
    let (mut lower, mut upper) = (0, boundaries.len() - 1);
    while lower < upper {
        let middle = (lower + upper).div_ceil(2);
        let candidate = format!("{}…", &label[..boundaries[middle]]);
        if measure(&candidate) <= max_width {
            lower = middle;
        } else {
            upper = middle - 1;
        }
    }
    (format!("{}…", &label[..boundaries[lower]]), true)
}

fn toc_label_rect(row: Rect, toggle: Rect) -> Rect {
    // Keep an arrow-sized leading slot even for leaf entries so labels align
    // with expandable rows instead of crowding the sidebar edge.
    let left = toggle.right() + 2.0;
    Rect::from_min_max(
        Pos2::new(left, row.top()),
        Pos2::new(row.right() - 8.0, row.bottom()),
    )
}

#[allow(clippy::cast_precision_loss)]
fn toc_row_top(row_index: usize, item_spacing: f32) -> f32 {
    row_index as f32 * (TOC_ROW_HEIGHT + item_spacing)
}

#[allow(clippy::cast_precision_loss)]
fn toc_content_height(row_count: usize, item_spacing: f32) -> f32 {
    if row_count == 0 {
        0.0
    } else {
        row_count as f32 * (TOC_ROW_HEIGHT + item_spacing) - item_spacing
    }
}

fn centered_toc_scroll_offset(row_index: usize, item_spacing: f32, viewport_height: f32) -> f32 {
    toc_row_top(row_index, item_spacing) - (viewport_height - TOC_ROW_HEIGHT).max(0.0) * 0.5
}

fn toc_navigation_keeps_bottom_offset(
    viewport: Rect,
    content_height: f32,
    row_index: usize,
    item_spacing: f32,
) -> bool {
    let maximum_offset = (content_height - viewport.height()).max(0.0);
    viewport.max.y + 1.0 >= content_height
        && centered_toc_scroll_offset(row_index, item_spacing, viewport.height()) + 1.0
            >= maximum_offset
}

fn update_toc_scroll_marker(
    marker: &mut Option<String>,
    preserve_bottom_after_navigation: bool,
    current_active: Option<&str>,
    rendered_active: Option<String>,
    should_auto_scroll: bool,
    active_row_found: bool,
) {
    if preserve_bottom_after_navigation {
        *marker = current_active.map(str::to_owned);
    } else if rendered_active.is_none() || (should_auto_scroll && active_row_found) {
        *marker = rendered_active;
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn stable_virtual_row_range(
    viewport: Rect,
    row_stride: f32,
    total_rows: usize,
) -> std::ops::Range<usize> {
    let min_row = ((viewport.min.y.max(0.0) / row_stride).floor() as usize).min(total_rows);
    let max_row = ((viewport.max.y.max(0.0) / row_stride).ceil() as usize)
        .saturating_add(1)
        .min(total_rows)
        .max(min_row);
    min_row..max_row
}

fn toc_toggle_button(
    ui: &mut egui::Ui,
    center: Pos2,
    id: &str,
    expanded: bool,
    selected: bool,
    collapse_text: &str,
    expand_text: &str,
) -> bool {
    let rect = Rect::from_center_size(center, Vec2::splat(26.0));
    let response = ui
        .interact(rect, ui.id().with(("toc-toggle", id)), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if expanded { collapse_text } else { expand_text });
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 6.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    paint_icon(
        ui,
        Rect::from_center_size(center, Vec2::splat(15.0)),
        if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        },
        if selected {
            palette().accent
        } else {
            palette().text
        },
    );
    response.clicked()
}

fn assistant_composer_keys(
    ui: &mut egui::Ui,
    input_id: egui::Id,
    initial_suggestion_count: usize,
) -> AssistantComposerKeys {
    let input_had_focus = ui.memory(|memory| memory.has_focus(input_id));
    let (arrow_down, arrow_up, tab, enter) = ui.input(|input| {
        (
            input.key_pressed(egui::Key::ArrowDown),
            input.key_pressed(egui::Key::ArrowUp),
            input.key_pressed(egui::Key::Tab),
            input.key_pressed(egui::Key::Enter),
        )
    });
    if input_had_focus && initial_suggestion_count > 0 {
        ui.input_mut(|input| {
            if arrow_down {
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
            }
            if arrow_up {
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
            }
            if tab {
                input.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
            }
            if enter {
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            }
        });
    }
    AssistantComposerKeys {
        input_had_focus,
        initial_suggestion_count,
        movement: if arrow_down {
            AssistantSuggestionMovement::Forward
        } else if arrow_up {
            AssistantSuggestionMovement::Backward
        } else {
            AssistantSuggestionMovement::None
        },
        acceptance: if tab {
            AssistantSuggestionAcceptance::Tab
        } else if enter {
            AssistantSuggestionAcceptance::Enter
        } else {
            AssistantSuggestionAcceptance::None
        },
    }
}

fn assistant_keyboard_scroll_input(
    ui: &mut egui::Ui,
    input_text: &str,
    cursor_char_index: usize,
    references: &[ChatReference],
) -> Option<f32> {
    (!assistant_suggestions_active(input_text, cursor_char_index, references)).then(|| {
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                -ASSISTANT_KEYBOARD_SCROLL_STEP
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                ASSISTANT_KEYBOARD_SCROLL_STEP
            } else {
                0.0
            }
        })
    })
}

fn assistant_suggestions_active(
    input_text: &str,
    cursor_char_index: usize,
    references: &[ChatReference],
) -> bool {
    chat_reference_token(input_text, cursor_char_index, references).is_some()
        || !chat_command_suggestions(input_text).is_empty()
}

fn active_suggestion_count(references: &[ChatReference], commands: &[ChatCommand]) -> usize {
    if references.is_empty() {
        commands.len()
    } else {
        references.len()
    }
}

fn chat_reference_chips(
    ui: &mut egui::Ui,
    references: &[ChatReference],
    language: crate::preferences::AppLanguage,
) -> Option<String> {
    if references.is_empty() {
        return None;
    }
    let mut removed = None;
    ui.horizontal_wrapped(|ui| {
        for reference in references {
            let kind = chat_reference_kind_label(language, reference.kind);
            let label = format!("{kind} · {}  ×", reference.label);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(label)
                            .size(crate::ui::scaled_font_size(10.5))
                            .color(palette().accent),
                    )
                    .fill(palette().accent_soft)
                    .stroke(egui::Stroke::new(1.0, palette().border))
                    .corner_radius(10),
                )
                .on_hover_text(&reference.description)
                .clicked()
            {
                removed = Some(reference.id.clone());
            }
        }
    });
    ui.add_space(3.0);
    removed
}

fn chat_reference_kind_label(
    language: crate::preferences::AppLanguage,
    kind: ChatReferenceKind,
) -> &'static str {
    match (language.resolved(), kind) {
        (crate::preferences::AppLanguage::SimplifiedChinese, ChatReferenceKind::Book) => "全文",
        (crate::preferences::AppLanguage::SimplifiedChinese, ChatReferenceKind::Section) => "章节",
        (crate::preferences::AppLanguage::SimplifiedChinese, ChatReferenceKind::Paragraph) => {
            "段落"
        }
        (crate::preferences::AppLanguage::English, ChatReferenceKind::Book) => "Book",
        (crate::preferences::AppLanguage::English, ChatReferenceKind::Section) => "Chapter",
        (crate::preferences::AppLanguage::English, ChatReferenceKind::Paragraph) => "Paragraph",
        (crate::preferences::AppLanguage::System, _) => unreachable!(),
    }
}

fn assistant_suggestion_popup(
    ui: &egui::Ui,
    anchor: Rect,
    references: &[ChatReference],
    commands: &[ChatCommand],
    selected_index: usize,
    language: crate::preferences::AppLanguage,
) -> (Option<ChatReference>, Option<ChatCommand>, Option<usize>) {
    let mut picked_reference = None;
    let mut picked_command = None;
    let mut hovered_index = None;
    let context = ui.ctx().clone();
    egui::Area::new("assistant-chat-suggestions".into())
        .order(egui::Order::Tooltip)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .fixed_pos(Pos2::new(anchor.left(), anchor.top() - 7.0))
        .show(&context, |ui| {
            egui::Frame::new()
                .fill(palette().surface)
                .stroke(egui::Stroke::new(1.0, palette().border))
                .corner_radius(8)
                .inner_margin(4)
                .show(ui, |ui| {
                    ui.set_width((anchor.width() - 8.0).max(1.0));
                    if references.is_empty() {
                        for (index, command) in commands.iter().enumerate() {
                            let label = format!("{}  {}", command.name, command.description);
                            let response =
                                navigation_text_button(ui, &label, index == selected_index);
                            if response.hovered() {
                                hovered_index = Some(index);
                            }
                            if response.clicked() {
                                picked_command = Some(*command);
                            }
                        }
                    } else {
                        for (index, reference) in references.iter().enumerate() {
                            let label = chat_reference_suggestion_label(reference, language);
                            let response =
                                navigation_text_button(ui, &label, index == selected_index)
                                    .on_hover_text(&reference.description);
                            if response.hovered() {
                                hovered_index = Some(index);
                            }
                            if response.clicked() {
                                picked_reference = Some(reference.clone());
                            }
                        }
                    }
                });
        });
    (picked_reference, picked_command, hovered_index)
}

fn chat_reference_suggestion_label(
    reference: &ChatReference,
    language: crate::preferences::AppLanguage,
) -> String {
    let kind = chat_reference_kind_label(language, reference.kind);
    if reference.kind != ChatReferenceKind::Book {
        return format!("{kind}  {}", reference.label);
    }
    let fallback = match language.resolved() {
        crate::preferences::AppLanguage::SimplifiedChinese => "整本书",
        crate::preferences::AppLanguage::English => "Entire book",
        crate::preferences::AppLanguage::System => unreachable!(),
    };
    if reference.description == fallback {
        kind.to_owned()
    } else {
        format!("{kind}  {}", reference.description)
    }
}

fn centered_assistant_text_edit(
    ui: &mut egui::Ui,
    input: &mut String,
    input_id: egui::Id,
    width: f32,
    hint_text: &str,
) -> (egui::text_edit::TextEditOutput, Rect) {
    let mut input_rect = Rect::NOTHING;
    let output = ui.allocate_ui_with_layout(
        Vec2::new(width, ASSISTANT_INPUT_HEIGHT),
        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
        |ui| {
            input_rect = ui.max_rect();
            egui::TextEdit::singleline(input)
                .id(input_id)
                .desired_width(width)
                .frame(egui::Frame::NONE)
                .vertical_align(egui::Align::Center)
                .hint_text(hint_text)
                .show(ui)
        },
    );
    (output.inner, input_rect)
}

fn page_texture_destination(page_rect: Rect, texture_size: Vec2) -> Rect {
    Rect::from_min_size(page_rect.min, texture_size)
}

fn compact_input_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(palette().surface)
        .stroke(egui::Stroke::new(1.0, palette().border))
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(8, 4))
}

fn focus_assistant_frame(inner_margin: egui::Margin) -> egui::Frame {
    egui::Frame::new()
        .fill(palette().surface)
        .stroke(egui::Stroke::new(1.0, palette().border))
        .corner_radius(12)
        .inner_margin(inner_margin)
        .shadow(egui::Shadow {
            offset: [0, 5],
            blur: 20,
            spread: 0,
            color: Color32::from_black_alpha(36),
        })
}

fn focus_data_indicator(ui: &mut egui::Ui, icon: Icon, hover_text: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::click());
    let fill = if response.hovered() {
        palette().accent_soft
    } else {
        palette().surface.gamma_multiply(0.94)
    };
    ui.painter().circle(
        rect.center(),
        15.0,
        fill,
        egui::Stroke::new(1.0, palette().border),
    );
    paint_icon(
        ui,
        rect.shrink(8.0),
        icon,
        palette()
            .accent
            .gamma_multiply(if response.hovered() { 1.0 } else { 0.72 }),
    );
    response.on_hover_text(hover_text).clicked()
}

fn focus_unit_index_for_scroll_offset(
    rects: impl IntoIterator<Item = Rect>,
    offset: f32,
    viewport_height: f32,
) -> Option<usize> {
    rects
        .into_iter()
        .enumerate()
        .map(|(index, rect)| {
            let target = focus_unit_target_offset_for_rect(rect, viewport_height);
            (index, (target - offset).abs())
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(index, _)| index)
}

fn focus_actions_overlay_y(note_open: bool, anchor_y: f32, viewport: Rect) -> f32 {
    if !note_open {
        return (anchor_y - 64.0).clamp(viewport.top() + 12.0, viewport.bottom() - 140.0);
    }
    let estimated_height = ASSISTANT_INPUT_HEIGHT + 12.0;
    (anchor_y - estimated_height / 2.0).clamp(
        viewport.top() + 12.0,
        (viewport.bottom() - estimated_height - 12.0).max(viewport.top() + 12.0),
    )
}

fn search_result_card(
    ui: &mut egui::Ui,
    result: &BookSearchResult,
    language: AppLanguage,
) -> egui::Response {
    let width = ui.available_width().max(1.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 76.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if response.is_pointer_button_down_on() || response.hovered() {
        palette().surface_muted
    } else {
        palette().surface
    };
    let stroke = if response.hovered() {
        egui::Stroke::new(1.0, palette().accent.gamma_multiply(0.4))
    } else {
        egui::Stroke::new(1.0, palette().border)
    };
    ui.painter()
        .rect(rect, 7.0, fill, stroke, egui::StrokeKind::Inside);

    let heading = search_result_heading(result, language);
    let content_rect = rect.shrink2(Vec2::new(10.0, 8.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(heading)
                                .size(crate::ui::scaled_font_size(11.0))
                                .strong()
                                .color(palette().muted),
                        )
                        .truncate(),
                    );
                });
                let mut excerpt_job = egui::text::LayoutJob::default();
                RichText::new(&result.excerpt)
                    .size(crate::ui::scaled_font_size(12.0))
                    .color(palette().text)
                    .append_to(
                        &mut excerpt_job,
                        ui.style(),
                        egui::FontSelection::Default,
                        egui::Align::Center,
                    );
                excerpt_job.wrap.max_width = ui.available_width();
                excerpt_job.wrap.max_rows = 2;
                ui.add(egui::Label::new(excerpt_job).wrap());
            });
        },
    );
    response
}

fn search_result_heading(result: &BookSearchResult, language: AppLanguage) -> String {
    let section = result.section_title.trim();
    let section = if section.is_empty() {
        language.text("正文", "Text")
    } else {
        section
    };
    section.to_owned()
}

fn selection_popover_frame(inner_margin: i8) -> egui::Frame {
    egui::Frame::new()
        .fill(palette().surface)
        .stroke(egui::Stroke::new(1.0, palette().border))
        .corner_radius(10)
        .inner_margin(inner_margin)
        .shadow(egui::Shadow {
            offset: [0, 4],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(28),
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AnnotationEditorAction {
    None,
    Save,
    Cancel,
}

fn focus_annotation_editor(
    ui: &mut egui::Ui,
    draft: &mut AnnotationDraft,
    language: crate::preferences::AppLanguage,
) -> AnnotationEditorAction {
    let input_id = ui.make_persistent_id("focus-annotation-input");
    let mut action = AnnotationEditorAction::None;
    let mut input_response = None;
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.horizontal(|ui| {
        let text_width = (ui.available_width() - 38.0).max(48.0);
        let line_height =
            ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().extra_text_line_spacing;
        let visual_rows = if draft.note.is_empty() {
            1
        } else {
            ui.painter()
                .layout(
                    draft.note.clone(),
                    egui::TextStyle::Body.resolve(ui.style()),
                    palette().text,
                    text_width,
                )
                .rows
                .len()
                .max(1)
        };
        let visible_rows = u8::try_from(visual_rows.min(4)).unwrap_or(4);
        let editor_margin = if visible_rows == 1 {
            egui::Margin::symmetric(0, 5)
        } else {
            egui::Margin::ZERO
        };
        let input_height = line_height * f32::from(visible_rows)
            + f32::from(editor_margin.top)
            + f32::from(editor_margin.bottom);
        let output = egui::ScrollArea::vertical()
            .id_salt("focus-annotation-scroll")
            .max_width(text_width)
            .max_height(input_height)
            .min_scrolled_width(text_width)
            .min_scrolled_height(input_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(text_width);
                egui::TextEdit::multiline(&mut draft.note)
                    .id(input_id)
                    .desired_width(text_width)
                    .desired_rows(1)
                    .frame(egui::Frame::NONE)
                    .margin(editor_margin)
                    .text_color(palette().text)
                    .hint_text(
                        language.text("写下这一刻的想法", "Write down what you are thinking"),
                    )
                    .show(ui)
            });
        input_response = Some(output.inner.response.response.clone());
        ui.add_enabled_ui(!draft.note.trim().is_empty(), |ui| {
            if icon_button(ui, Icon::MessageSquareText)
                .on_hover_text(language.text("保存批注", "Save note"))
                .clicked()
            {
                action = AnnotationEditorAction::Save;
            }
        });
    });
    if draft.focus_pending {
        input_response
            .expect("focus annotation input is always rendered")
            .request_focus();
        draft.focus_pending = false;
    }
    action
}

fn annotation_editor(
    ui: &mut egui::Ui,
    draft: &mut AnnotationDraft,
    language: crate::preferences::AppLanguage,
) -> AnnotationEditorAction {
    let mut action = AnnotationEditorAction::None;
    ui.set_width(312.0);
    if annotation_text_editor(ui, draft, language) {
        action = AnnotationEditorAction::Cancel;
    }
    ui.add_space(10.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if annotation_action_button(
            ui,
            language.text("保存", "Save"),
            true,
            !draft.note.trim().is_empty(),
        )
        .clicked()
        {
            action = AnnotationEditorAction::Save;
        }
    });
    action
}

fn annotation_text_editor(
    ui: &mut egui::Ui,
    draft: &mut AnnotationDraft,
    language: crate::preferences::AppLanguage,
) -> bool {
    let input_id = ui.make_persistent_id("selection-annotation-input");
    let mut close_clicked = false;
    let input = egui::Frame::new()
        .fill(palette().surface)
        .corner_radius(7)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.horizontal_top(|ui| {
                let text_width = (ui.available_width() - 32.0).max(1.0);
                let input = ui.add_sized(
                    [text_width, 72.0],
                    egui::TextEdit::multiline(&mut draft.note)
                        .id(input_id)
                        .frame(egui::Frame::NONE)
                        .margin(0)
                        .text_color(palette().text)
                        .hint_text(
                            language.text("写下这一刻的想法", "Write down what you are thinking"),
                        ),
                );
                close_clicked = small_icon_button(ui, Icon::X)
                    .on_hover_text(language.text("关闭", "Close"))
                    .clicked();
                input
            })
            .inner
        })
        .inner;
    if draft.focus_pending {
        input.request_focus();
        draft.focus_pending = false;
    }
    close_clicked
}

fn annotation_action_button(
    ui: &mut egui::Ui,
    label: &str,
    primary: bool,
    enabled: bool,
) -> egui::Response {
    let size = Vec2::new(68.0, 30.0);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let colors = palette();
    let fill = if !enabled {
        colors.surface_muted
    } else if primary && response.hovered() {
        colors.accent.gamma_multiply(0.88)
    } else if response.hovered() {
        colors.active_fill
    } else if primary {
        colors.accent
    } else {
        colors.surface
    };
    let stroke = if enabled && primary {
        egui::Stroke::NONE
    } else {
        egui::Stroke::new(1.0, colors.border)
    };
    let text_color = if !enabled {
        colors.muted
    } else if primary {
        Color32::WHITE
    } else {
        colors.text
    };
    ui.painter()
        .rect(rect, 6, fill, stroke, egui::StrokeKind::Inside);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(crate::ui::scaled_font_size(12.0)),
        text_color,
    );
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

fn clipped_annotation_action_text(value: &str) -> String {
    let mut clipped = value.chars().take(48).collect::<String>();
    if value.chars().count() > 48 {
        clipped.push('…');
    }
    clipped
}

fn capture_clicked_citation(current: &mut Option<String>, clicked: Option<String>) {
    if let Some(locator) = clicked {
        *current = Some(locator);
    }
}

fn auto_scroll_assistant_selection(
    ctx: &egui::Context,
    output: &egui::containers::scroll_area::ScrollAreaOutput<()>,
) {
    let has_label_selection = ctx
        .plugin::<egui::text_selection::LabelSelectionState>()
        .lock()
        .has_selection();
    let Some((pointer, stable_dt)) = ctx.input(|input| {
        (has_label_selection && input.pointer.primary_down())
            .then(|| {
                input
                    .pointer
                    .interact_pos()
                    .map(|pointer| (pointer, input.stable_dt))
            })
            .flatten()
    }) else {
        return;
    };
    let delta = assistant_selection_autoscroll_delta(pointer, output.inner_rect, stable_dt);
    if delta == 0.0 {
        return;
    }

    let max_offset = (output.content_size.y - output.inner_rect.height()).max(0.0);
    let mut state = output.state;
    let next_offset = (state.offset.y + delta).clamp(0.0, max_offset);
    if (next_offset - state.offset.y).abs() <= f32::EPSILON {
        return;
    }
    state.offset.y = next_offset;
    state.store(ctx, output.id);
    ctx.request_repaint();
}

fn assistant_selection_autoscroll_delta(pointer: Pos2, viewport: Rect, stable_dt: f32) -> f32 {
    if pointer.x < viewport.left() || pointer.x > viewport.right() {
        return 0.0;
    }
    let (direction, distance) = if pointer.y < viewport.top() + ASSISTANT_SELECTION_SCROLL_EDGE {
        (
            -1.0,
            viewport.top() + ASSISTANT_SELECTION_SCROLL_EDGE - pointer.y,
        )
    } else if pointer.y > viewport.bottom() - ASSISTANT_SELECTION_SCROLL_EDGE {
        (
            1.0,
            pointer.y - (viewport.bottom() - ASSISTANT_SELECTION_SCROLL_EDGE),
        )
    } else {
        return 0.0;
    };
    let strength = (distance / ASSISTANT_SELECTION_SCROLL_EDGE).clamp(0.0, 1.0);
    let speed = ASSISTANT_SELECTION_SCROLL_MIN_SPEED
        + (ASSISTANT_SELECTION_SCROLL_MAX_SPEED - ASSISTANT_SELECTION_SCROLL_MIN_SPEED)
            * strength
            * strength;
    direction * speed * stable_dt.min(0.05)
}

fn chat_message_card(
    ui: &mut egui::Ui,
    role: ChatRole,
    content: &str,
    language: crate::preferences::AppLanguage,
    markdown: &mut ChatMarkdownState,
    message_ordinal: usize,
    streaming: bool,
) -> Option<String> {
    let is_user = role == ChatRole::User;
    let width = ui.available_width();
    let mut clicked_citation = None;
    egui::Frame::new()
        .fill(if is_user {
            palette().accent_soft
        } else {
            palette().surface
        })
        .stroke(egui::Stroke::new(
            1.0,
            if is_user {
                palette().accent_border
            } else {
                palette().border
            },
        ))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(10, 9))
        .show(ui, |ui| {
            ui.set_min_width((width - 20.0).max(1.0));
            if is_user {
                ui.add(
                    egui::Label::new(
                        RichText::new(content)
                            .size(crate::ui::scaled_font_size(12.5))
                            .color(palette().text),
                    )
                    .wrap()
                    .selectable(true),
                );
            } else {
                clicked_citation = markdown.show(ui, content, language, message_ordinal, streaming);
            }
        });
    clicked_citation
}

fn preview_wheel_delta(input: &egui::InputState) -> f32 {
    input
        .raw
        .events
        .iter()
        .filter_map(|event| match event {
            egui::Event::MouseWheel { unit, delta, .. } => Some(
                delta.y
                    * match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 40.0,
                        egui::MouseWheelUnit::Page => 240.0,
                    },
            ),
            _ => None,
        })
        .sum()
}

impl SelectedImage {
    pub(super) fn from_reader_image(
        image: &ReaderImage,
        scroll_mode: bool,
    ) -> Result<Self, &'static str> {
        let (Ok(width), Ok(height)) = (usize::try_from(image.width), usize::try_from(image.height))
        else {
            return Err("Image dimensions exceed the clipboard limit");
        };
        let Some(byte_len) = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            return Err("Image dimensions exceed the clipboard limit");
        };
        if width == 0 || height == 0 || image.pixels.len() < byte_len {
            return Err("Image data is incomplete and cannot be copied");
        }
        if image.display_width <= 0.0 || image.display_height <= 0.0 {
            return Err("Image has invalid display bounds");
        }

        Ok(Self {
            image: egui::ColorImage::from_rgba_unmultiplied(
                [width, height],
                &image.pixels[..byte_len],
            ),
            position: image.position,
            bounds: Rect::from_min_size(
                Pos2::new(image.x, image.y),
                Vec2::new(image.display_width, image.display_height),
            ),
            scroll_mode,
        })
    }
}

fn consume_copy_shortcut(input: &mut egui::InputState, shortcut: &egui::KeyboardShortcut) -> bool {
    let native_copy = egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::C);
    (*shortcut == native_copy && consume_copy_event(&mut input.events))
        || input.consume_shortcut(shortcut)
}

fn focus_unit_copy_text(
    focus_body_active: bool,
    text_edit_focused: bool,
    current_unit_is_image: bool,
    current_text: Option<&str>,
) -> Option<&str> {
    if !focus_body_active || text_edit_focused || current_unit_is_image {
        return None;
    }
    current_text.map(str::trim).filter(|text| !text.is_empty())
}

fn consume_copy_event(events: &mut Vec<egui::Event>) -> bool {
    let Some(index) = events
        .iter()
        .position(|event| matches!(event, egui::Event::Copy))
    else {
        return false;
    };
    events.remove(index);
    true
}

fn shortcut_tooltip(
    language: AppLanguage,
    simplified_chinese: &str,
    english: &str,
    shortcut: &str,
) -> String {
    match language.resolved() {
        AppLanguage::SimplifiedChinese => format!("{simplified_chinese}（{shortcut}）"),
        AppLanguage::English => format!("{english} ({shortcut})"),
        AppLanguage::System => unreachable!(),
    }
}

struct ImagePreviewInteraction {
    close: bool,
    reset: bool,
    drag_delta: Vec2,
}

fn show_image_preview_area(
    ctx: &egui::Context,
    screen: Rect,
    image_rect: Rect,
    texture_id: TextureId,
    zoom_percent: f32,
) -> ImagePreviewInteraction {
    let mut interaction = ImagePreviewInteraction {
        close: false,
        reset: false,
        drag_delta: Vec2::ZERO,
    };
    egui::Area::new("reader-image-preview".into())
        .order(egui::Order::Tooltip)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            // The backdrop only owns clicks used to close the preview. Registering it for
            // dragging as well competes with the image's drag response on the same layer.
            let (backdrop_rect, backdrop) =
                ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(backdrop_rect, 0.0, Color32::from_black_alpha(190));
            ui.painter().image(
                texture_id,
                image_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            ui.painter().rect_stroke(
                image_rect,
                2.0,
                egui::Stroke::new(1.0, Color32::from_white_alpha(48)),
                egui::StrokeKind::Inside,
            );

            let image_response = ui
                .interact(
                    image_rect,
                    ui.id().with("preview-image"),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::Grab);
            if image_response.dragged() {
                interaction.drag_delta = ctx.input(|input| input.pointer.delta());
            }
            interaction.reset = image_response.double_clicked();

            ui.painter().text(
                Pos2::new(screen.center().x.round(), (screen.bottom() - 24.0).round()),
                egui::Align2::CENTER_BOTTOM,
                format!("{zoom_percent:.0}%"),
                egui::FontId::monospace(crate::ui::scaled_font_size(12.0)),
                Color32::WHITE,
            );
            if backdrop.clicked()
                && backdrop
                    .interact_pointer_pos()
                    .is_some_and(|position| !image_rect.contains(position))
            {
                interaction.close = true;
            }
        });
    interaction
}

fn zoom_from_wheel(zoom: f32, wheel_delta: f32) -> f32 {
    (zoom * (wheel_delta * IMAGE_PREVIEW_WHEEL_SPEED).exp())
        .clamp(IMAGE_PREVIEW_MIN_ZOOM, IMAGE_PREVIEW_MAX_ZOOM)
}

fn clamp_preview_pan(pan: Vec2, display_size: Vec2, available: Vec2) -> Vec2 {
    let limit = Vec2::new(
        (display_size.x - available.x).abs() / 2.0,
        (display_size.y - available.y).abs() / 2.0,
    );
    Vec2::new(
        pan.x.clamp(-limit.x, limit.x),
        pan.y.clamp(-limit.y, limit.y),
    )
}

fn color32(color: rebook_publication::Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(color.red, color.green, color.blue, color.alpha)
}

#[allow(clippy::cast_possible_truncation)]
fn unit_f32(value: f64) -> f32 {
    value.clamp(0.0, 1.0) as f32
}

fn toolbar_title_x(toolbar_rect: Rect, content_left: f32, toolbar_visible: bool) -> f32 {
    if toolbar_visible {
        toolbar_rect.center().x
    } else {
        toolbar_rect.left() + content_left
    }
}

fn paint_toolbar_title(
    ui: &egui::Ui,
    toolbar_rect: Rect,
    content_left: f32,
    toolbar_visible: bool,
    title: &str,
) {
    let title_x = toolbar_title_x(toolbar_rect, content_left, toolbar_visible);
    let title_inset = (TOOLBAR_CONTROL_SIZE * 3.0 + f32::from(SIDEBAR_PADDING))
        .min((toolbar_rect.width() / 2.0 - 1.0).max(0.0));
    let title_clip = if toolbar_visible {
        toolbar_rect.shrink2(Vec2::new(title_inset, 0.0))
    } else {
        Rect::from_min_max(
            Pos2::new(toolbar_rect.left() + content_left, toolbar_rect.top()),
            toolbar_rect.max,
        )
    };
    ui.painter().with_clip_rect(title_clip).text(
        Pos2::new(title_x, toolbar_rect.center().y),
        if toolbar_visible {
            egui::Align2::CENTER_CENTER
        } else {
            egui::Align2::LEFT_CENTER
        },
        title,
        egui::FontId::proportional(crate::ui::scaled_font_size(TOOLBAR_TITLE_SIZE)),
        palette().text,
    );
}

fn last_visible_selection_rect(
    rects: &[rebook_reader::ReaderSelectionRect],
    positions: &[rebook_reader::ReaderPosition],
) -> Option<rebook_reader::ReaderSelectionRect> {
    rects
        .iter()
        .rev()
        .find(|rect| positions.contains(&rect.position))
        .copied()
}

fn page_wheel_input_allowed(pointer_over_page: bool, blocked: bool) -> bool {
    pointer_over_page && !blocked
}

#[cfg(test)]
mod reference_suggestion_label_tests {
    use super::*;

    fn reference(kind: ChatReferenceKind, label: &str, description: &str) -> ChatReference {
        ChatReference {
            id: "test".into(),
            kind,
            label: label.into(),
            description: description.into(),
            link: "link://test".into(),
            excerpt: None,
        }
    }

    #[test]
    fn streaming_message_citations_are_forwarded_to_navigation() {
        let mut clicked = None;

        capture_clicked_citation(&mut clicked, Some("link://j/3/n4".into()));

        assert_eq!(clicked.as_deref(), Some("link://j/3/n4"));
    }

    #[test]
    fn focus_annotation_editor_uses_content_height_for_an_empty_draft() {
        egui::__run_test_ui(|ui| {
            ui.set_max_height(300.0);
            let mut draft = AnnotationDraft {
                note: String::new(),
                focus_pending: false,
            };
            let response = focus_assistant_frame(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.set_width(318.0);
                focus_annotation_editor(
                    ui,
                    &mut draft,
                    crate::preferences::AppLanguage::SimplifiedChinese,
                )
            });

            assert!(response.response.rect.height() < 72.0);
        });
    }

    #[test]
    fn focus_scrollbar_offset_targets_the_nearest_paragraph() {
        let rects = [
            Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(300.0, 80.0)),
            Rect::from_min_size(Pos2::new(0.0, 320.0), Vec2::new(300.0, 80.0)),
            Rect::from_min_size(Pos2::new(0.0, 720.0), Vec2::new(300.0, 80.0)),
        ];

        assert_eq!(
            focus_unit_index_for_scroll_offset(rects, 300.0, 600.0),
            Some(1)
        );
        assert_eq!(
            focus_unit_index_for_scroll_offset(rects, 690.0, 600.0),
            Some(2)
        );
    }

    #[test]
    fn focus_mode_shortcuts_match_the_reader_contract() {
        let shortcuts = crate::preferences::ShortcutPreferences::default();
        assert_eq!(
            shortcuts.search,
            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::F)
        );
        assert_eq!(
            shortcuts.copy,
            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::C)
        );
        assert_eq!(
            shortcuts.toggle_cursor,
            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::H)
        );
        assert_eq!(
            shortcuts.focus_actions,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Space)
        );
        assert_eq!(
            shortcuts.focus_chat,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Tab)
        );
        assert_eq!(
            shortcuts.focus_highlight,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Num1)
        );
        assert_eq!(
            shortcuts.focus_note,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Num2)
        );
        assert_eq!(
            shortcuts.focus_structure,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Num3)
        );
        assert_eq!(
            shortcuts.focus_footnotes,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::AltLeft)
        );
        assert_eq!(
            shortcuts.previous_section,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::ArrowLeft)
        );
        assert_eq!(
            shortcuts.next_section,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::ArrowRight)
        );
        assert_eq!(
            shortcuts.previous_page_or_paragraph,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::ArrowUp)
        );
        assert_eq!(
            shortcuts.next_page_or_paragraph,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::ArrowDown)
        );
        assert_eq!(
            shortcuts.focus_extend_selection_previous,
            egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::ArrowUp)
        );
        assert_eq!(
            shortcuts.focus_extend_selection_next,
            egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::ArrowDown)
        );
    }

    #[test]
    fn classic_left_and_right_switch_reading_units() {
        assert_eq!(
            classic_navigation_action(true, false, false, false, false, false, false),
            Some(ClassicNavigationAction::PreviousReadingUnit)
        );
        assert_eq!(
            classic_navigation_action(false, true, false, false, false, false, false),
            Some(ClassicNavigationAction::NextReadingUnit)
        );
    }

    #[test]
    fn classic_vertical_navigation_still_turns_pages() {
        assert_eq!(
            classic_navigation_action(false, false, true, false, false, false, false),
            Some(ClassicNavigationAction::PreviousPage)
        );
        assert_eq!(
            classic_navigation_action(false, false, false, true, false, false, false),
            Some(ClassicNavigationAction::NextPage)
        );
    }

    #[test]
    fn toc_keyboard_navigation_starts_from_the_active_row_and_stays_in_bounds() {
        assert_eq!(
            next_toc_keyboard_row(None, Some(2), 5, PageDirection::Next),
            Some(3)
        );
        assert_eq!(
            next_toc_keyboard_row(None, Some(2), 5, PageDirection::Previous),
            Some(1)
        );
        assert_eq!(
            next_toc_keyboard_row(Some(4), Some(2), 5, PageDirection::Next),
            Some(4)
        );
        assert_eq!(
            next_toc_keyboard_row(Some(0), Some(2), 5, PageDirection::Previous),
            Some(0)
        );
        assert_eq!(
            next_toc_keyboard_row(None, None, 5, PageDirection::Next),
            Some(0)
        );
        assert_eq!(
            next_toc_keyboard_row(None, None, 5, PageDirection::Previous),
            Some(4)
        );
        assert_eq!(
            next_toc_keyboard_row(None, None, 0, PageDirection::Next),
            None
        );
    }

    #[test]
    fn pinned_classic_toc_leaves_vertical_navigation_to_the_reader() {
        assert!(!toc_keyboard_navigation_enabled(false, true));
        assert!(toc_keyboard_navigation_enabled(false, false));
        assert!(toc_keyboard_navigation_enabled(true, true));
    }

    #[test]
    fn horizontal_toc_navigation_only_changes_expandable_rows() {
        assert_eq!(
            toc_expansion_target(TocKeyboardAction::Expand, true, false),
            Some(true)
        );
        assert_eq!(
            toc_expansion_target(TocKeyboardAction::Collapse, true, true),
            Some(false)
        );
        assert_eq!(
            toc_expansion_target(TocKeyboardAction::Expand, true, true),
            None
        );
        assert_eq!(
            toc_expansion_target(TocKeyboardAction::Collapse, true, false),
            None
        );
        assert_eq!(
            toc_expansion_target(TocKeyboardAction::Expand, false, false),
            None
        );
        assert_eq!(
            toc_expansion_target(TocKeyboardAction::Collapse, false, false),
            None
        );
    }

    #[test]
    fn bare_alt_triggers_only_after_an_uninterrupted_release() {
        let mut state = ModifierTapState::Idle;
        assert!(!advance_modifier_tap(
            &mut state,
            ModifierTapInput::CleanPress
        ));
        assert_eq!(state, ModifierTapState::Armed);
        assert!(advance_modifier_tap(&mut state, ModifierTapInput::Release));
        assert_eq!(state, ModifierTapState::Idle);

        assert!(!advance_modifier_tap(
            &mut state,
            ModifierTapInput::CleanPress
        ));
        assert!(!advance_modifier_tap(
            &mut state,
            ModifierTapInput::OtherInput
        ));
        assert_eq!(state, ModifierTapState::Cancelled);
        assert!(!advance_modifier_tap(&mut state, ModifierTapInput::Release));
        assert_eq!(state, ModifierTapState::Idle);
    }

    #[test]
    fn bare_alt_candidate_is_cancelled_by_chords_and_focus_loss() {
        let mut state = ModifierTapState::Idle;
        assert!(!advance_modifier_tap(
            &mut state,
            ModifierTapInput::ChordedPress
        ));
        assert!(!advance_modifier_tap(&mut state, ModifierTapInput::Release));

        assert!(!advance_modifier_tap(
            &mut state,
            ModifierTapInput::CleanPress
        ));
        assert!(!advance_modifier_tap(
            &mut state,
            ModifierTapInput::FocusLost
        ));
        assert_eq!(state, ModifierTapState::Idle);
        assert!(!advance_modifier_tap(&mut state, ModifierTapInput::Release));
    }

    #[test]
    fn modifier_tap_mode_is_limited_to_the_bare_left_alt_binding() {
        assert!(is_bare_left_alt_shortcut(egui::KeyboardShortcut::new(
            egui::Modifiers::NONE,
            egui::Key::AltLeft
        )));
        assert!(!is_bare_left_alt_shortcut(egui::KeyboardShortcut::new(
            egui::Modifiers::ALT,
            egui::Key::F
        )));
        assert!(!is_bare_left_alt_shortcut(egui::KeyboardShortcut::new(
            egui::Modifiers::NONE,
            egui::Key::AltRight
        )));
    }

    #[test]
    fn cursor_hiding_is_scoped_to_the_unblocked_focus_reader() {
        assert!(crate::reader::resolved_focus_cursor_hidden(true, None));
        assert!(!crate::reader::resolved_focus_cursor_hidden(
            true,
            Some(false)
        ));
        assert!(crate::reader::resolved_focus_cursor_hidden(
            false,
            Some(true)
        ));
        assert!(should_hide_reader_cursor(true, true, false, false));
        assert!(!should_hide_reader_cursor(false, true, false, false));
        assert!(!should_hide_reader_cursor(true, false, false, false));
        assert!(!should_hide_reader_cursor(true, true, true, false));
        assert!(!should_hide_reader_cursor(true, true, false, true));
    }

    #[test]
    fn escape_only_requests_close_for_the_reader_menu() {
        assert!(reader_menu_close_requested(ReaderOverlay::Menu, true));
        assert!(!reader_menu_close_requested(ReaderOverlay::Menu, false));
        assert!(!reader_menu_close_requested(ReaderOverlay::None, true));
    }

    #[test]
    fn assistant_selection_autoscroll_only_activates_near_vertical_edges() {
        let viewport = Rect::from_min_size(Pos2::new(20.0, 100.0), Vec2::new(300.0, 400.0));
        let frame_dt = 1.0 / 60.0;

        assert!(
            assistant_selection_autoscroll_delta(
                Pos2::new(120.0, viewport.top() + 4.0),
                viewport,
                frame_dt,
            ) < 0.0
        );
        assert!(
            assistant_selection_autoscroll_delta(
                Pos2::new(120.0, viewport.bottom() - 4.0),
                viewport,
                frame_dt,
            ) > 0.0
        );
        assert!(
            assistant_selection_autoscroll_delta(viewport.center(), viewport, frame_dt).abs()
                <= f32::EPSILON
        );
        assert!(
            assistant_selection_autoscroll_delta(
                Pos2::new(viewport.right() + 1.0, viewport.bottom()),
                viewport,
                frame_dt,
            )
            .abs()
                <= f32::EPSILON
        );
    }

    #[test]
    fn chapter_title_aligns_with_content_when_hidden_and_centers_when_toolbar_is_visible() {
        let toolbar = Rect::from_min_size(Pos2::new(20.0, 10.0), Vec2::new(1000.0, 48.0));

        assert!((toolbar_title_x(toolbar, 140.0, false) - 160.0).abs() <= f32::EPSILON);
        assert!((toolbar_title_x(toolbar, 140.0, true) - 520.0).abs() <= f32::EPSILON);
    }

    #[test]
    fn only_original_pdf_layout_tracks_the_page_image_left_edge() {
        assert!(uses_pdf_page_alignment(
            rebook_formats::BookFormat::Pdf,
            PdfOcrViewMode::Original,
        ));
        assert!(!uses_pdf_page_alignment(
            rebook_formats::BookFormat::Pdf,
            PdfOcrViewMode::Reflow,
        ));
        assert!(!uses_pdf_page_alignment(
            rebook_formats::BookFormat::Epub,
            PdfOcrViewMode::Original,
        ));
    }

    #[test]
    fn modifier_tap_event_stream_distinguishes_alt_from_alt_chords() {
        let key = |key, pressed, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        };
        let mut state = ModifierTapState::Idle;
        assert!(modifier_tap_triggered(
            &mut state,
            &[
                key(egui::Key::AltLeft, true, egui::Modifiers::ALT),
                key(egui::Key::AltLeft, false, egui::Modifiers::NONE),
            ],
            egui::Key::AltLeft,
        ));

        assert!(!modifier_tap_triggered(
            &mut state,
            &[
                key(egui::Key::AltLeft, true, egui::Modifiers::ALT),
                key(egui::Key::F, true, egui::Modifiers::ALT),
                key(egui::Key::F, false, egui::Modifiers::ALT),
                key(egui::Key::AltLeft, false, egui::Modifiers::NONE),
            ],
            egui::Key::AltLeft,
        ));

        assert!(!modifier_tap_triggered(
            &mut state,
            &[
                key(egui::Key::AltLeft, true, egui::Modifiers::ALT),
                key(egui::Key::Tab, true, egui::Modifiers::ALT),
                egui::Event::WindowFocused(false),
            ],
            egui::Key::AltLeft,
        ));
        assert_eq!(state, ModifierTapState::Idle);
    }

    #[test]
    fn modifier_tap_event_stream_rejects_altgr_and_other_modifiers() {
        let key = |key, pressed, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        };
        let ctrl_alt = egui::Modifiers::CTRL | egui::Modifiers::ALT;
        let mut state = ModifierTapState::Idle;
        assert!(!modifier_tap_triggered(
            &mut state,
            &[
                key(egui::Key::ControlLeft, true, egui::Modifiers::CTRL),
                key(egui::Key::AltRight, true, ctrl_alt),
                key(egui::Key::AltRight, false, egui::Modifiers::CTRL),
                key(egui::Key::ControlLeft, false, egui::Modifiers::NONE),
            ],
            egui::Key::AltLeft,
        ));
        assert_eq!(state, ModifierTapState::Idle);

        assert!(!modifier_tap_triggered(
            &mut state,
            &[
                key(egui::Key::AltLeft, true, ctrl_alt),
                key(egui::Key::AltLeft, false, egui::Modifiers::CTRL),
            ],
            egui::Key::AltLeft,
        ));
        assert_eq!(state, ModifierTapState::Idle);
    }

    #[test]
    fn ocr_reflow_images_support_preview_without_treating_pdf_pages_as_images() {
        assert!(!supports_image_preview(
            rebook_formats::BookFormat::Pdf,
            PdfOcrViewMode::Original,
        ));
        assert!(supports_image_preview(
            rebook_formats::BookFormat::Pdf,
            PdfOcrViewMode::Reflow,
        ));
        assert!(supports_image_preview(
            rebook_formats::BookFormat::Epub,
            PdfOcrViewMode::Original,
        ));
    }

    #[test]
    fn selection_toolbar_anchors_to_the_last_rect_on_the_visible_spread() {
        let position = |page_index| rebook_reader::ReaderPosition {
            section_index: 0,
            segment_index: 0,
            page_index,
        };
        let rect = |page_index, x| rebook_reader::ReaderSelectionRect {
            position: position(page_index),
            x,
            y: 20.0,
            width: 30.0,
            height: 18.0,
        };
        let rects = [rect(0, 40.0), rect(1, 650.0), rect(2, 40.0)];

        assert_eq!(
            last_visible_selection_rect(&rects, &[position(0), position(1)]),
            Some(rects[1])
        );
    }

    #[test]
    fn page_wheel_remains_available_when_a_selection_toolbar_overlays_the_page() {
        // The toolbar lives in a foreground Area, so the page response itself is
        // no longer `hovered`. Physical containment is the relevant condition.
        assert!(page_wheel_input_allowed(true, false));
    }

    #[test]
    fn page_wheel_stays_blocked_by_modal_reader_interactions() {
        assert!(!page_wheel_input_allowed(false, false));
        assert!(!page_wheel_input_allowed(true, true));
    }

    #[test]
    fn reference_rows_show_only_user_facing_content() {
        let language = crate::preferences::AppLanguage::SimplifiedChinese;
        assert_eq!(
            chat_reference_suggestion_label(
                &reference(ChatReferenceKind::Book, "全文", "Structured Writing"),
                language,
            ),
            "全文  Structured Writing"
        );
        assert_eq!(
            chat_reference_suggestion_label(
                &reference(
                    ChatReferenceKind::Section,
                    "Chapter 7. Rhetorical Structure",
                    "当前章节 · 7",
                ),
                language,
            ),
            "章节  Chapter 7. Rhetorical Structure"
        );
        assert_eq!(
            chat_reference_suggestion_label(
                &reference(ChatReferenceKind::Book, "全文", "整本书"),
                language,
            ),
            "全文"
        );
    }

    #[test]
    fn assistant_text_edit_uses_the_full_centered_input_row() {
        egui::__run_test_ui(|ui| {
            let mut input = String::new();
            let input_id = ui.make_persistent_id("centered-input-test");
            let (output, input_rect) =
                centered_assistant_text_edit(ui, &mut input, input_id, 240.0, "Ask this book");
            let galley_center = output.galley_pos.y + output.galley.size().y / 2.0;

            assert!((output.response.rect.height() - ASSISTANT_INPUT_HEIGHT).abs() < 0.01);
            assert!((output.response.rect.center().y - input_rect.center().y).abs() < 0.01);
            assert!((galley_center - input_rect.center().y).abs() < 0.01);
        });
    }

    #[test]
    fn long_toc_labels_are_elided_to_the_sidebar_row() {
        fn measured_width(text: &str) -> f32 {
            f32::from(u16::try_from(text.chars().count()).unwrap_or(u16::MAX)) * 10.0
        }

        let original = "Separate font system that any application can use";
        let (display, elided) = elide_text_to_width(original, 170.0, measured_width);

        assert!(elided);
        assert!(display.ends_with('…'));
        assert!(measured_width(&display) <= 170.0);
        assert!(display.len() < original.len());
    }

    #[test]
    fn toc_click_at_bottom_suppresses_only_the_next_reposition() {
        let mut marker = Some("old".into());

        update_toc_scroll_marker(
            &mut marker,
            true,
            Some("clicked"),
            Some("old".into()),
            false,
            true,
        );

        assert_eq!(marker.as_deref(), Some("clicked"));
    }

    #[test]
    fn toc_click_away_from_bottom_keeps_normal_animated_repositioning() {
        let mut marker = Some("old".into());

        update_toc_scroll_marker(
            &mut marker,
            false,
            Some("clicked"),
            Some("old".into()),
            false,
            true,
        );

        assert_eq!(marker.as_deref(), Some("old"));
    }

    #[test]
    fn only_bottom_pinned_rows_suppress_repositioning_at_the_end() {
        let item_spacing = 4.0;
        let row_count = 2_034;
        let viewport_height = 400.0;
        let content_height = toc_content_height(row_count, item_spacing);
        let viewport = Rect::from_min_max(
            Pos2::new(0.0, content_height - viewport_height),
            Pos2::new(240.0, content_height),
        );

        assert!(toc_navigation_keeps_bottom_offset(
            viewport,
            content_height,
            row_count - 1,
            item_spacing,
        ));
        assert!(!toc_navigation_keeps_bottom_offset(
            viewport,
            content_height,
            row_count - 10,
            item_spacing,
        ));
    }

    #[test]
    fn virtual_toc_range_does_not_backfill_rows_at_the_bottom_boundary() {
        let row_stride = 40.0;
        let total_rows = 2_034;
        let viewport_before_boundary =
            Rect::from_min_max(Pos2::new(0.0, 80_940.0), Pos2::new(240.0, 81_319.9));
        let viewport_after_boundary =
            Rect::from_min_max(Pos2::new(0.0, 80_940.0), Pos2::new(240.0, 81_320.1));

        assert_eq!(
            stable_virtual_row_range(viewport_before_boundary, row_stride, total_rows),
            2_023..2_034
        );
        assert_eq!(
            stable_virtual_row_range(viewport_after_boundary, row_stride, total_rows),
            2_023..2_034
        );
    }

    #[test]
    fn stale_page_texture_keeps_its_size_and_starts_at_the_moving_canvas() {
        let page_rect = Rect::from_min_size(Pos2::new(256.0, 48.0), Vec2::new(944.0, 700.0));
        let previous_texture_size = Vec2::new(1_200.0, 700.0);

        let destination = page_texture_destination(page_rect, previous_texture_size);

        assert!((destination.left() - page_rect.left()).abs() < 0.01);
        assert!((destination.top() - page_rect.top()).abs() < 0.01);
        assert!((destination.width() - previous_texture_size.x).abs() < 0.01);
        assert!((destination.height() - previous_texture_size.y).abs() < 0.01);
    }

    #[test]
    fn side_panel_widths_leave_room_for_reader_content() {
        assert_eq!(
            constrained_panel_widths(1_200.0, SIDEBAR_WIDTH, ASSISTANT_WIDTH, true, true),
            (SIDEBAR_WIDTH, ASSISTANT_WIDTH)
        );
        assert_eq!(
            constrained_panel_widths(720.0, SIDEBAR_MAX_WIDTH, ASSISTANT_MAX_WIDTH, true, true,),
            (SIDEBAR_MIN_WIDTH, ASSISTANT_MIN_WIDTH)
        );
        assert_eq!(
            constrained_panel_widths(720.0, SIDEBAR_MAX_WIDTH, ASSISTANT_MAX_WIDTH, false, true,),
            (SIDEBAR_MAX_WIDTH, 520.0)
        );
    }

    #[test]
    fn image_preview_zoom_is_bounded_and_pan_stays_within_visible_edges() {
        assert!(
            (zoom_from_wheel(IMAGE_PREVIEW_MAX_ZOOM, 240.0) - IMAGE_PREVIEW_MAX_ZOOM).abs()
                < f32::EPSILON
        );
        assert!(
            (zoom_from_wheel(IMAGE_PREVIEW_MIN_ZOOM, -240.0) - IMAGE_PREVIEW_MIN_ZOOM).abs()
                < f32::EPSILON
        );
        assert_eq!(
            clamp_preview_pan(
                Vec2::new(500.0, -500.0),
                Vec2::new(1_200.0, 900.0),
                Vec2::new(800.0, 700.0),
            ),
            Vec2::new(200.0, -100.0)
        );
        assert_eq!(
            clamp_preview_pan(
                Vec2::new(20.0, 20.0),
                Vec2::new(400.0, 300.0),
                Vec2::new(800.0, 700.0),
            ),
            Vec2::new(20.0, 20.0)
        );
        assert_eq!(
            clamp_preview_pan(
                Vec2::new(500.0, -500.0),
                Vec2::new(400.0, 300.0),
                Vec2::new(800.0, 700.0),
            ),
            Vec2::new(200.0, -200.0)
        );
    }

    #[test]
    fn copy_shortcut_consumes_the_high_level_egui_copy_event() {
        let mut events = vec![
            egui::Event::Text("keep".into()),
            egui::Event::Copy,
            egui::Event::Text("also keep".into()),
        ];

        assert!(consume_copy_event(&mut events));
        assert_eq!(
            events,
            vec![
                egui::Event::Text("keep".into()),
                egui::Event::Text("also keep".into()),
            ]
        );
        assert!(!consume_copy_event(&mut events));
    }

    #[test]
    fn focus_copy_uses_the_current_text_when_the_reader_body_is_active() {
        assert_eq!(
            focus_unit_copy_text(true, false, false, Some("  current paragraph  ")),
            Some("current paragraph")
        );
        assert_eq!(focus_unit_copy_text(true, false, false, Some("  ")), None);
    }

    #[test]
    fn focus_copy_does_not_override_text_editing_or_inactive_reader_shortcuts() {
        assert_eq!(
            focus_unit_copy_text(true, true, false, Some("current paragraph")),
            None
        );
        assert_eq!(
            focus_unit_copy_text(false, false, false, Some("current paragraph")),
            None
        );
        assert_eq!(
            focus_unit_copy_text(true, false, true, Some("image description")),
            None
        );
    }

    #[test]
    fn selected_reader_image_keeps_original_pixels_and_display_bounds() {
        let image = ReaderImage {
            position: rebook_reader::ReaderPosition {
                section_index: 2,
                segment_index: 3,
                page_index: 4,
            },
            x: 24.0,
            y: 36.0,
            display_width: 120.0,
            display_height: 80.0,
            width: 2,
            height: 1,
            pixels: std::sync::Arc::from([255, 0, 0, 255, 0, 255, 0, 255]),
        };

        let selected = SelectedImage::from_reader_image(&image, true).expect("valid image");

        assert_eq!(selected.image.size, [2, 1]);
        assert_eq!(selected.bounds.min, Pos2::new(24.0, 36.0));
        assert_eq!(selected.bounds.size(), Vec2::new(120.0, 80.0));
        assert!(selected.scroll_mode);
    }

    #[test]
    fn selected_image_border_stays_below_foreground_modals() {
        assert_eq!(selected_image_layer_id().order, egui::Order::Middle);
    }
}
