//! First production-shaped GPUI desktop surface: the real local shelf.

mod text_input;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    AnyElement, App, Application, Bounds, ClipboardItem, Context, Entity, FocusHandle, Image,
    ImageFormat, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    Render, ScrollHandle, SharedString, StyledImage as _, Window, WindowBounds, WindowOptions,
    actions, div, img, point, prelude::*, px, rgb, rgba, size,
};
use rebook_assistant::{
    AssistantAnnotationAction, AssistantRuntime, ChatActivity, ChatReadingContext, ChatRole,
    ChatSelection, ChatSession, ChatSubmitError, PendingAnnotationActions, TranslationMode,
};
use rebook_formats::open_file_for_reading;
use rebook_gpui_renderer::{GpuiFramePresenter, GpuiSourceOverlay, GpuiTextEngine};
use rebook_layout::{LayoutViewport, ReaderStyle, SpreadMode, TypesettingMode, text::TextEngine};
use rebook_library::{LibraryBook, LocalLibrary};
use rebook_publication::{BookSource, RenditionLayout, SourceAnchor, SourceRange};
use rebook_reader::{
    NavigationOutcome, PageDirection, ReaderFocusCommand, ReaderFocusLayoutPolicy,
    ReaderFocusState, ReaderFocusTransition, ReaderFocusUnit, ReaderFocusViewportPolicy,
    ReaderPosition, ReaderScrollLayout, ReaderSelection, ReaderSession, ReaderTextCursor,
    ReaderTextHit, ReadingMode, SelectionGranularity, TextCursorDirection, TocViewItem,
};
use rebook_session::{
    DocumentAssistantToolHost, DocumentSourcePipeline, ReaderDocumentPreferenceChange,
    ReaderDocumentPreferences, StoredHighlightMutationTarget,
};
use rebook_sync::{HighlightStore, StoredHighlight, StoredProgress, SyncStore, open_default_store};

use crate::text_input::NoteInput;

actions!(
    torto_gpui_desktop,
    [
        RefreshShelf,
        BackToShelf,
        PreviousPage,
        NextPage,
        PreviousFocusUnit,
        NextFocusUnit,
        ToggleFocusActions,
        OpenFocusChat,
        SendFocusChat,
        CancelFocusChat,
        ConfirmFocusChatMutations,
        CancelFocusChatMutations,
        SaveFocusHighlight,
        OpenFocusAnnotation,
        ToggleFocusFootnotes,
        CopySelection,
        ToggleContents,
        ExtendSelectionLeft,
        ExtendSelectionRight,
        SelectCurrentPage,
        SaveHighlight,
        ToggleAnnotations,
        OpenAnnotation,
        SaveAnnotation,
        CancelAnnotation,
        ToggleReaderSettings,
        SaveReaderSettings,
        CancelReaderSettings
    ]
);

const READER_CHROME_HEIGHT: f32 = 88.0;
const READER_OUTER_PADDING: f32 = 32.0;
const MIN_READER_WIDTH: f32 = 360.0;
const MAX_READER_WIDTH: f32 = 800.0;
const MIN_READER_HEIGHT: f32 = 360.0;
const READER_TOC_WIDTH: f32 = 280.0;
const READER_ANNOTATIONS_WIDTH: f32 = 320.0;
const FOCUS_CHAT_TOOL_STEPS: usize = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ReaderSidebar {
    #[default]
    None,
    Contents,
    Annotations,
}

#[derive(Clone)]
struct ReaderSettingsEditor {
    draft: ReaderDocumentPreferences,
    error: Option<String>,
}

impl ReaderSettingsEditor {
    fn new(preferences: &ReaderDocumentPreferences) -> Self {
        Self {
            draft: preferences.clone(),
            error: None,
        }
    }

    fn apply(&mut self, change: ReaderDocumentPreferenceChange) {
        self.draft.apply(change);
        self.error = None;
    }
}

#[derive(Clone, Copy)]
enum ReaderSettingsAdjustment {
    FontSize(f32),
    LineHeight(f32),
}

struct AnnotationEditor {
    highlight_id: Option<String>,
    ranges: Vec<SourceRange>,
    quote: String,
    input: Entity<NoteInput>,
}

struct FocusChatEditor {
    range: SourceRange,
    session: ChatSession,
    input: Entity<NoteInput>,
    visible: bool,
    pending_annotation_actions: PendingAnnotationActions<StoredHighlight>,
    mutation_error: Option<String>,
}

struct ShelfBook {
    element_id: u64,
    title: SharedString,
    byline: SharedString,
    file_name: SharedString,
    publication_id: String,
    path: PathBuf,
    cover: Option<Arc<Image>>,
}

impl ShelfBook {
    fn from_library(book: &LibraryBook) -> Self {
        Self {
            element_id: u64::from_str_radix(&book.id[..book.id.len().min(16)], 16)
                .unwrap_or(book.added_at),
            title: book.title.clone().into(),
            byline: if book.authors.is_empty() {
                "未知作者".into()
            } else {
                book.authors.join("、").into()
            },
            file_name: book.file_name.clone().into(),
            publication_id: book.id.clone(),
            path: book.path.clone(),
            cover: book
                .cover_bytes
                .as_ref()
                .and_then(|bytes| cover_image(bytes)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PendingReaderScroll {
    content_y: f32,
    viewport_y: f32,
}

impl PendingReaderScroll {
    const TOP_INSET: f32 = 16.0;

    const fn near_top(content_y: f32) -> Self {
        Self {
            content_y,
            viewport_y: Self::TOP_INSET,
        }
    }
}

struct ReaderSurface {
    book_id: String,
    title: SharedString,
    document_sources: DocumentSourcePipeline,
    session: ReaderSession,
    presenter: GpuiFramePresenter,
    viewport: LayoutViewport,
    status: SharedString,
    selection: Option<ReaderSelection>,
    selection_anchor: Option<ReaderTextCursor>,
    selection_focus: Option<ReaderTextCursor>,
    drag_anchor: Option<ReaderTextHit>,
    dragging: bool,
    progress_dirty: bool,
    progress_store: SyncStore,
    highlight_store: HighlightStore,
    highlights: Vec<StoredHighlight>,
    scroll: ScrollHandle,
    scroll_layout: ReaderScrollLayout,
    focus_units: Vec<ReaderFocusUnit>,
    focus_state: ReaderFocusState,
    pending_scroll: Option<PendingReaderScroll>,
    visible_frame_index: usize,
    toc_items: Arc<[TocViewItem]>,
    sidebar: ReaderSidebar,
    annotation_editor: Option<AnnotationEditor>,
    focus_chat: Option<FocusChatEditor>,
    reading_mode: ReadingMode,
    selection_granularity: SelectionGranularity,
}

impl ReaderSurface {
    const fn pointer_selection_granularity(&self) -> SelectionGranularity {
        if matches!(self.reading_mode, ReadingMode::Focus) {
            SelectionGranularity::Paragraph
        } else if matches!(self.selection_granularity, SelectionGranularity::Free) {
            // A click still needs a visible semantic selection; unrestricted
            // character selection takes over once the pointer starts dragging.
            SelectionGranularity::Word
        } else {
            self.selection_granularity
        }
    }

    fn confirm_pending_annotation_actions(
        &mut self,
        pending: &mut PendingAnnotationActions<StoredHighlight>,
    ) -> Result<(), String> {
        let result = {
            let mut target = StoredHighlightMutationTarget::new(
                &mut self.highlight_store,
                self.highlights.iter().cloned(),
            );
            pending.confirm(&mut target).map(|_| ())
        };
        self.highlights = self.highlight_store.for_book(&self.book_id);
        result
    }

    fn apply_annotation_actions(
        &mut self,
        actions: &[AssistantAnnotationAction<StoredHighlight>],
    ) -> Result<(), String> {
        let mut pending = PendingAnnotationActions::from_actions(actions.to_vec());
        self.confirm_pending_annotation_actions(&mut pending)
    }

    fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_anchor = None;
        self.selection_focus = None;
        self.drag_anchor = None;
        self.dragging = false;
    }

    fn install_selection(&mut self, selection: ReaderSelection) -> Result<(), String> {
        let Some((anchor, focus)) = self
            .scroll_layout
            .cursors_for_source_ranges(&selection.ranges)
        else {
            self.clear_selection();
            return Err("当前阅读单元无法恢复选择光标".into());
        };
        self.selection = Some(selection);
        self.selection_anchor = Some(anchor);
        self.selection_focus = Some(focus);
        Ok(())
    }

    fn rebuild_scroll_layout(&mut self, anchor: Option<&SourceAnchor>) -> Result<(), String> {
        let location = self.session.location();
        let position = ReaderPosition {
            section_index: location.section_index,
            segment_index: location.segment_index,
            page_index: location.page_index,
        };
        let layout = self
            .session
            .current_scroll_layout(false)
            .map_err(|error| error.to_string())?;
        let focus_anchor = anchor
            .cloned()
            .or_else(|| self.focus_state.anchor().cloned());
        let (focus_units, focus_state) =
            compile_focus_presentation(&mut self.session, &layout, focus_anchor, position)?;
        let visible_frame_index = layout.frame_index(position).unwrap_or(0);
        let pending_scroll = anchor
            .and_then(|anchor| layout.source_anchor_top(anchor))
            .map(PendingReaderScroll::near_top)
            .or_else(|| focus_scroll_target(&focus_units, &focus_state, self.viewport))
            .or_else(|| {
                layout
                    .frame_top(position)
                    .map(PendingReaderScroll::near_top)
            });
        self.scroll_layout = layout;
        self.focus_units = focus_units;
        self.focus_state = focus_state;
        self.visible_frame_index = visible_frame_index;
        self.pending_scroll = pending_scroll;
        self.presenter.clear();
        Ok(())
    }

    fn restore_selection_ranges(&mut self, ranges: Option<Vec<SourceRange>>) {
        let Some(ranges) = ranges else {
            return;
        };
        let Some((anchor, focus)) = self.scroll_layout.cursors_for_source_ranges(&ranges) else {
            self.clear_selection();
            return;
        };
        match self.session.selection_between_cursors(anchor, focus) {
            Ok(selection) => {
                self.selection = selection;
                self.selection_anchor = Some(anchor);
                self.selection_focus = Some(focus);
            }
            Err(_) => self.clear_selection(),
        }
    }

    fn apply_document_preferences(
        &mut self,
        preferences: &ReaderDocumentPreferences,
    ) -> Result<(), String> {
        let fixed_page =
            self.session.book_source().book().metadata.layout == RenditionLayout::PrePaginated;
        let resolved = preferences.resolve(!fixed_page, fixed_page);
        let source_anchor = self.focus_state.anchor().cloned().or_else(|| {
            self.session
                .current_locator()
                .source
                .map(|range| range.start)
        });
        let selection_ranges = self
            .selection
            .as_ref()
            .map(|selection| selection.ranges.clone());
        let previous_style = self.session.style().clone();
        let previous_mode = self.reading_mode;
        let previous_granularity = self.selection_granularity;

        if let Err(error) = self.session.set_style(resolved.style) {
            let _ = self.session.set_style(previous_style);
            return Err(error.to_string());
        }
        self.reading_mode = resolved.presentation.mode;
        self.selection_granularity = resolved.selection_granularity;
        if let Err(error) = self.rebuild_scroll_layout(source_anchor.as_ref()) {
            let _ = self.session.set_style(previous_style);
            self.reading_mode = previous_mode;
            self.selection_granularity = previous_granularity;
            let _ = self.rebuild_scroll_layout(source_anchor.as_ref());
            self.restore_selection_ranges(selection_ranges);
            return Err(error);
        }
        self.restore_selection_ranges(selection_ranges);
        self.focus_state.hide_actions();
        self.focus_state.hide_footnotes();
        if let Some(chat) = self.focus_chat.as_mut() {
            chat.visible = false;
        }
        self.progress_dirty = true;
        Ok(())
    }

    fn hit_test_scroll(&self, window_x: f32, window_y: f32, exact: bool) -> Option<ReaderTextHit> {
        let frames = self.scroll_layout.frames();
        let frame_index = if exact {
            (0..frames.len()).find(|index| {
                self.scroll.bounds_for_item(*index).is_some_and(|bounds| {
                    window_y >= f32::from(bounds.top()) && window_y <= f32::from(bounds.bottom())
                })
            })?
        } else {
            (0..frames.len()).min_by(|left, right| {
                scroll_item_vertical_distance(&self.scroll, *left, window_y).total_cmp(
                    &scroll_item_vertical_distance(&self.scroll, *right, window_y),
                )
            })?
        };
        let bounds = self.scroll.bounds_for_item(frame_index)?;
        let x = window_x - f32::from(bounds.left());
        let height = frames[frame_index].height.max(1.0);
        let y = (window_y - f32::from(bounds.top())).clamp(0.0, height);
        self.scroll_layout.hit_test_text(frame_index, x, y, exact)
    }

    fn apply_pending_scroll(&mut self) {
        let Some(target) = self.pending_scroll else {
            return;
        };
        let Some((frame_index, _)) = self.scroll_layout.frame_at_content_y(target.content_y) else {
            self.pending_scroll = None;
            return;
        };
        let Some(item_bounds) = self.scroll.bounds_for_item(frame_index) else {
            self.scroll.scroll_to_top_of_item(frame_index);
            return;
        };
        let scroll_bounds = self.scroll.bounds();
        let current = self.scroll.offset();
        let unscrolled_top = item_bounds.top() - current.y;
        let local_y = target.content_y - self.scroll_layout.frames()[frame_index].top;
        let desired_y = scroll_bounds.top() + px(target.viewport_y);
        let max_offset = self.scroll.max_offset().height;
        let offset_y = (desired_y - unscrolled_top - px(local_y)).clamp(-max_offset, px(0.0));
        self.scroll.set_offset(point(current.x, offset_y));
        self.visible_frame_index = frame_index;
        self.pending_scroll = None;
    }

    fn sync_visible_scroll_position(&mut self) {
        self.apply_pending_scroll();
        if self.pending_scroll.is_some() || self.scroll_layout.frames().is_empty() {
            return;
        }
        let frame_index = self
            .scroll
            .top_item()
            .min(self.scroll_layout.frames().len() - 1);
        if frame_index == self.visible_frame_index {
            return;
        }
        let position = self.scroll_layout.frames()[frame_index].position;
        if self.session.set_visible_position(position).is_ok() {
            self.visible_frame_index = frame_index;
            self.progress_dirty = true;
        }
    }

    fn persist_progress(&mut self) -> Result<(), String> {
        if !self.progress_dirty {
            return Ok(());
        }
        let mut locator = self.session.current_locator();
        if let Some(unit) = self.focus_units.get(self.focus_state.active_index()) {
            locator.source = Some(unit.range.clone());
        }
        self.progress_store
            .save_progress(&self.book_id, &locator)
            .map_err(|error| error.to_string())?;
        self.progress_dirty = false;
        Ok(())
    }

    fn activate_focus_unit(&mut self, index: usize) -> Result<ReaderFocusTransition, String> {
        let transition = self
            .focus_state
            .apply(&self.focus_units, ReaderFocusCommand::Select(index));
        if !matches!(
            transition,
            ReaderFocusTransition::Selected(_) | ReaderFocusTransition::Unchanged
        ) {
            return Ok(transition);
        }
        let Some(unit) = self.focus_units.get(self.focus_state.active_index()) else {
            return Ok(ReaderFocusTransition::Empty);
        };
        self.pending_scroll =
            focus_scroll_target(&self.focus_units, &self.focus_state, self.viewport);
        self.session
            .set_visible_position(unit.geometry.position)
            .map_err(|error| error.to_string())?;
        self.visible_frame_index = self
            .scroll_layout
            .frame_index(unit.geometry.position)
            .unwrap_or(self.visible_frame_index);
        if let Some(chat) = self.focus_chat.as_mut() {
            chat.visible = false;
        }
        self.clear_selection();
        self.progress_dirty = true;
        Ok(transition)
    }

    fn active_focus_window_center_y(&self) -> Option<f32> {
        let unit = self.focus_units.get(self.focus_state.active_index())?;
        let frame_index = self.scroll_layout.frame_index(unit.geometry.position)?;
        let frame = self.scroll_layout.frames().get(frame_index)?;
        let item_bounds = self.scroll.bounds_for_item(frame_index)?;
        let local_center = unit.geometry.bounds.center().1 - frame.top;
        Some(f32::from(item_bounds.top()) + local_center)
    }

    fn active_focus_selection(
        &mut self,
    ) -> Result<(ReaderSelection, ReaderTextCursor, ReaderTextCursor), String> {
        let unit = self
            .focus_units
            .get(self.focus_state.active_index())
            .ok_or_else(|| "当前没有可操作的专注段落".to_owned())?;
        if unit.is_image() {
            return Err("图片暂不支持高亮或批注".into());
        }
        let ranges = if unit.paint_ranges.is_empty() {
            std::slice::from_ref(&unit.range)
        } else {
            unit.paint_ranges.as_slice()
        };
        let (anchor, focus) = self
            .scroll_layout
            .cursors_for_source_ranges(ranges)
            .ok_or_else(|| "当前段落无法恢复文本选择".to_owned())?;
        let selection = self
            .session
            .selection_between_cursors(anchor, focus)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "当前段落没有可选择的文字".to_owned())?;
        Ok((selection, anchor, focus))
    }

    fn install_active_focus_selection(&mut self) -> Result<(), String> {
        let (selection, anchor, focus) = self.active_focus_selection()?;
        self.selection = Some(selection);
        self.selection_anchor = Some(anchor);
        self.selection_focus = Some(focus);
        Ok(())
    }
}

fn scroll_item_vertical_distance(scroll: &ScrollHandle, index: usize, y: f32) -> f32 {
    let Some(bounds) = scroll.bounds_for_item(index) else {
        return f32::MAX;
    };
    let top = f32::from(bounds.top());
    let bottom = f32::from(bounds.bottom());
    if y < top {
        top - y
    } else if y > bottom {
        y - bottom
    } else {
        0.0
    }
}

impl Drop for ReaderSurface {
    fn drop(&mut self) {
        let _ = self.persist_progress();
    }
}

struct Shelf {
    books: Vec<ShelfBook>,
    load_error: Option<String>,
    status: SharedString,
    reader: Option<ReaderSurface>,
    focus_handle: FocusHandle,
    reader_preferences: ReaderDocumentPreferences,
    reader_settings: Option<ReaderSettingsEditor>,
}

impl Shelf {
    fn reader_setting_choice(
        id: &'static str,
        label: &'static str,
        selected: bool,
        change: ReaderDocumentPreferenceChange,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(if selected { 0x009A_C6B1 } else { 0x00D1_D7D3 }))
            .bg(rgb(if selected { 0x00E2_F0E9 } else { 0x00FF_FFFD }))
            .text_color(rgb(0x0036_403B))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x00E7_EDEA)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.change_reader_setting(change.clone(), cx);
                }),
            )
            .child(label)
            .into_any_element()
    }

    fn reader_setting_adjustment(
        id: &'static str,
        label: &'static str,
        adjustment: ReaderSettingsAdjustment,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .w(px(34.0))
            .h(px(34.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x00D1_D7D3))
            .bg(rgb(0x00FF_FFFD))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x00E7_EDEA)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| match adjustment {
                    ReaderSettingsAdjustment::FontSize(delta) => {
                        this.adjust_reader_font_size(delta, cx);
                    }
                    ReaderSettingsAdjustment::LineHeight(delta) => {
                        this.adjust_reader_line_height(delta, cx);
                    }
                }),
            )
            .child(label)
            .into_any_element()
    }

    fn load(focus_handle: FocusHandle) -> Self {
        let reader_preferences = ReaderDocumentPreferences::load_default().unwrap_or_default();
        match load_books() {
            Ok(books) => {
                let count = books.len();
                Self {
                    books,
                    load_error: None,
                    status: format!("已读取现有书库，共 {count} 本").into(),
                    reader: None,
                    focus_handle,
                    reader_preferences,
                    reader_settings: None,
                }
            }
            Err(error) => Self {
                books: Vec::new(),
                load_error: Some(error.clone()),
                status: format!("读取书库失败：{error}").into(),
                reader: None,
                focus_handle,
                reader_preferences,
                reader_settings: None,
            },
        }
    }

    fn refresh(&mut self, _: &RefreshShelf, _: &mut Window, cx: &mut Context<Self>) {
        match load_books() {
            Ok(books) => {
                let count = books.len();
                self.books = books;
                self.load_error = None;
                self.status = format!("书架已刷新，共 {count} 本").into();
            }
            Err(error) => {
                self.load_error = Some(error.clone());
                self.status = format!("刷新失败：{error}").into();
            }
        }
        cx.notify();
    }

    fn toggle_reader_settings(
        &mut self,
        _: &ToggleReaderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader.is_none() {
            return;
        }
        if self.reader_settings.is_some() {
            self.reader_settings = None;
        } else {
            if self
                .reader
                .as_ref()
                .is_some_and(|reader| reader.annotation_editor.is_some())
            {
                if let Some(reader) = self.reader.as_mut() {
                    reader.status = "请先保存或关闭当前批注".into();
                }
                cx.notify();
                return;
            }
            self.reader_settings = Some(ReaderSettingsEditor::new(&self.reader_preferences));
            if let Some(reader) = self.reader.as_mut() {
                reader.focus_state.hide_actions();
                reader.focus_state.hide_footnotes();
                if let Some(chat) = reader.focus_chat.as_mut() {
                    chat.visible = false;
                }
            }
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn cancel_reader_settings(
        &mut self,
        _: &CancelReaderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reader_settings = None;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn change_reader_setting(
        &mut self,
        change: ReaderDocumentPreferenceChange,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.reader_settings.as_mut() {
            editor.apply(change);
            cx.notify();
        }
    }

    fn adjust_reader_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let Some(editor) = self.reader_settings.as_ref() else {
            return;
        };
        let mut typography = editor.draft.typography.clone();
        typography.font_size += delta;
        self.change_reader_setting(ReaderDocumentPreferenceChange::Typography(typography), cx);
    }

    fn adjust_reader_line_height(&mut self, delta: f32, cx: &mut Context<Self>) {
        let Some(editor) = self.reader_settings.as_ref() else {
            return;
        };
        let mut typesetting = editor.draft.typesetting.clone();
        typesetting.body_line_height += delta;
        self.change_reader_setting(ReaderDocumentPreferenceChange::Typesetting(typesetting), cx);
    }

    fn save_reader_settings(
        &mut self,
        _: &SaveReaderSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(next) = self
            .reader_settings
            .as_ref()
            .map(|editor| editor.draft.clone())
        else {
            return;
        };
        let previous = self.reader_preferences.clone();
        if next == previous {
            self.reader_settings = None;
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        if let Some(reader) = self.reader.as_mut()
            && let Err(error) = reader.apply_document_preferences(&next)
        {
            if let Some(editor) = self.reader_settings.as_mut() {
                editor.error = Some(format!("重新排版失败：{error}"));
            }
            cx.notify();
            return;
        }
        if let Err(error) = next.save_default() {
            let rollback_error = self
                .reader
                .as_mut()
                .and_then(|reader| reader.apply_document_preferences(&previous).err());
            if let Some(editor) = self.reader_settings.as_mut() {
                editor.error = Some(rollback_error.map_or_else(
                    || format!("保存设置失败：{error}"),
                    |rollback| format!("保存设置失败：{error}；恢复原版式失败：{rollback}"),
                ));
            }
            cx.notify();
            return;
        }
        self.reader_preferences = next;
        self.reader_settings = None;
        if let Some(reader) = self.reader.as_mut() {
            reader.status = "阅读设置已保存并重新排版".into();
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn open_book(
        &mut self,
        path: &Path,
        publication_id: &str,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = reader_viewport(window, 0.0);
        let text_system = Arc::clone(window.text_system());
        let progress_store = match open_default_store() {
            Ok(store) => store,
            Err(error) => {
                self.status = format!("无法打开阅读数据库：{error}").into();
                cx.notify();
                return;
            }
        };
        match open_reader_session(
            path,
            publication_id,
            title,
            viewport,
            || GpuiTextEngine::new(Arc::clone(&text_system)),
            progress_store,
            &self.reader_preferences,
        ) {
            Ok(reader) => {
                self.reader = Some(reader);
                self.status = format!("已打开《{title}》").into();
            }
            Err(error) => {
                self.status = format!("无法打开《{title}》：{error}").into();
            }
        }
        cx.notify();
    }

    fn back_to_shelf(&mut self, _: &BackToShelf, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader_settings.take().is_some() {
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        if let Some(reader) = self.reader.as_mut() {
            let chat_visible = reader.focus_chat.as_ref().is_some_and(|chat| chat.visible);
            if reader.focus_state.actions_visible()
                || reader.focus_state.footnotes_visible()
                || chat_visible
            {
                reader.focus_state.hide_actions();
                reader.focus_state.hide_footnotes();
                if let Some(chat) = reader.focus_chat.as_mut() {
                    chat.visible = false;
                }
                window.focus(&self.focus_handle);
                cx.notify();
                return;
            }
        }
        let progress_error = self
            .reader
            .as_mut()
            .and_then(|reader| reader.persist_progress().err());
        self.reader = None;
        self.status = progress_error
            .map_or_else(
                || format!("已返回书架，共 {} 本", self.books.len()),
                |error| format!("已返回书架，但保存阅读进度失败：{error}"),
            )
            .into();
        cx.notify();
    }

    fn turn_reading_unit(
        &mut self,
        direction: PageDirection,
        enter_from_focus_boundary: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        match reader.session.go_to_adjacent_reading_unit(direction) {
            Ok(result) => {
                let location = result.snapshot.location;
                reader.status = if result.outcome == NavigationOutcome::Boundary {
                    match direction {
                        PageDirection::Previous => "已经是第一个阅读单元".into(),
                        PageDirection::Next => "已经是最后一个阅读单元".into(),
                    }
                } else {
                    format!(
                        "第 {} 节 · 从第 {}/{} 页开始",
                        location.section_index + 1,
                        location.page_index + 1,
                        location.page_count
                    )
                    .into()
                };
                reader.clear_selection();
                if result.outcome != NavigationOutcome::Boundary {
                    let anchor = reader.session.current_reading_unit_anchor();
                    if let Err(error) = reader.rebuild_scroll_layout(anchor.as_ref()) {
                        reader.status = format!("{} · 连续布局失败：{error}", reader.status).into();
                    } else if enter_from_focus_boundary && !reader.focus_units.is_empty() {
                        let index = match direction {
                            PageDirection::Previous => reader.focus_units.len() - 1,
                            PageDirection::Next => 0,
                        };
                        if let Err(error) = reader.activate_focus_unit(index) {
                            reader.status =
                                format!("{} · 专注定位失败：{error}", reader.status).into();
                        }
                    }
                    reader.progress_dirty = true;
                    if let Err(error) = reader.persist_progress() {
                        reader.status = format!("{} · 保存进度失败：{error}", reader.status).into();
                    }
                }
            }
            Err(error) => reader.status = format!("翻页失败：{error}").into(),
        }
        cx.notify();
    }

    fn turn_page(&mut self, direction: PageDirection, cx: &mut Context<Self>) {
        if self.reader_settings.is_some() {
            return;
        }
        self.turn_reading_unit(direction, false, cx);
    }

    fn move_focus(&mut self, direction: PageDirection, cx: &mut Context<Self>) {
        if self.reader_settings.is_some() {
            return;
        }
        if self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.reading_mode != ReadingMode::Focus)
        {
            return;
        }
        let transition = {
            let Some(reader) = self.reader.as_mut() else {
                return;
            };
            reader
                .focus_state
                .apply(&reader.focus_units, ReaderFocusCommand::Move(direction))
        };
        match transition {
            ReaderFocusTransition::Boundary(boundary) => {
                self.turn_reading_unit(boundary, true, cx);
                return;
            }
            ReaderFocusTransition::Selected(index) => {
                let reader = self
                    .reader
                    .as_mut()
                    .expect("focus transition requires an active reader");
                if let Err(error) = reader.activate_focus_unit(index) {
                    reader.status = format!("专注段落定位失败：{error}").into();
                } else {
                    reader.status = format!(
                        "专注段落 {}/{}",
                        reader.focus_state.active_index() + 1,
                        reader.focus_units.len()
                    )
                    .into();
                    if let Err(error) = reader.persist_progress() {
                        reader.status = format!("{} · 保存进度失败：{error}", reader.status).into();
                    }
                }
            }
            ReaderFocusTransition::Empty => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.status = "当前阅读单元没有可聚焦的正文".into();
                }
            }
            ReaderFocusTransition::Unchanged => {}
        }
        cx.notify();
    }

    fn previous_page(&mut self, _: &PreviousPage, _: &mut Window, cx: &mut Context<Self>) {
        self.turn_page(PageDirection::Previous, cx);
    }

    fn next_page(&mut self, _: &NextPage, _: &mut Window, cx: &mut Context<Self>) {
        self.turn_page(PageDirection::Next, cx);
    }

    fn previous_focus_unit(
        &mut self,
        _: &PreviousFocusUnit,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_focus(PageDirection::Previous, cx);
    }

    fn next_focus_unit(&mut self, _: &NextFocusUnit, _: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(PageDirection::Next, cx);
    }

    fn toggle_focus_footnotes(
        &mut self,
        _: &ToggleFocusFootnotes,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        if reader.reading_mode != ReadingMode::Focus {
            return;
        }
        if reader.focus_state.footnotes_visible() {
            reader.focus_state.hide_footnotes();
        } else if reader
            .focus_units
            .get(reader.focus_state.active_index())
            .is_some_and(|unit| !unit.footnotes.is_empty())
        {
            reader.focus_state.show_footnotes();
            reader.clear_selection();
        } else {
            reader.status = "当前段落没有脚注".into();
        }
        cx.notify();
    }

    fn toggle_focus_actions(
        &mut self,
        _: &ToggleFocusActions,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        if reader.reading_mode != ReadingMode::Focus {
            return;
        }
        if reader.annotation_editor.is_some() {
            return;
        }
        if let Some(chat) = reader.focus_chat.as_mut() {
            chat.visible = false;
        }
        if reader.focus_state.actions_visible() {
            reader.focus_state.hide_actions();
        } else {
            reader.focus_state.show_actions();
            reader.clear_selection();
        }
        cx.notify();
    }

    fn open_focus_chat(&mut self, _: &OpenFocusChat, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader_settings.is_some() {
            return;
        }
        let prepared = (|| {
            let reader = self.reader.as_mut()?;
            if !reader.focus_state.actions_visible() {
                return None;
            }
            reader.focus_state.hide_actions();
            let range = reader
                .focus_units
                .get(reader.focus_state.active_index())?
                .range
                .clone();
            let (selection, _, _) = reader.active_focus_selection().ok()?;
            let existing_input = reader
                .focus_chat
                .as_ref()
                .filter(|chat| chat.range == range)
                .map(|chat| chat.input.clone());
            Some((range, selection, existing_input))
        })();
        let Some((range, selection, existing_input)) = prepared else {
            if let Some(reader) = self.reader.as_mut() {
                reader.status = "当前段落无法作为 AI 对话上下文".into();
            }
            cx.notify();
            return;
        };
        if let Some(input) = existing_input {
            let focus_handle = input.read(cx).focus_handle();
            if let Some(reader) = self.reader.as_mut()
                && let Some(chat) = reader.focus_chat.as_mut()
            {
                chat.visible = true;
                chat.session.set_selection(Some(ChatSelection {
                    text: selection.text,
                    ranges: selection.ranges,
                }));
            }
            window.focus(&focus_handle);
            cx.notify();
            return;
        }
        if self.reader.as_ref().is_some_and(|reader| {
            reader.focus_chat.as_ref().is_some_and(|chat| {
                chat.range != range && !chat.pending_annotation_actions.is_empty()
            })
        }) {
            if let Some(reader) = self.reader.as_mut() {
                reader.status = "上一段落仍有待确认的 AI 批注操作，请先返回处理".into();
            }
            cx.notify();
            return;
        }
        let input = cx.new(|cx| NoteInput::chat(cx.focus_handle(), "", "问问当前段落"));
        let focus_handle = input.read(cx).focus_handle();
        if let Some(reader) = self.reader.as_mut() {
            let session_id = u64::try_from(reader.focus_state.active_index())
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let mut session = ChatSession::new(session_id);
            session.set_selection(Some(ChatSelection {
                text: selection.text,
                ranges: selection.ranges,
            }));
            reader.focus_chat = Some(FocusChatEditor {
                range,
                session,
                input,
                visible: true,
                pending_annotation_actions: PendingAnnotationActions::new(),
                mutation_error: None,
            });
        }
        window.focus(&focus_handle);
        cx.notify();
    }

    fn send_focus_chat(&mut self, _: &SendFocusChat, _: &mut Window, cx: &mut Context<Self>) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(input) = reader
            .focus_chat
            .as_ref()
            .filter(|chat| chat.visible)
            .map(|chat| chat.input.clone())
        else {
            return;
        };
        if reader
            .focus_chat
            .as_ref()
            .is_some_and(|chat| !chat.pending_annotation_actions.is_empty())
        {
            reader.status = "请先确认或取消 AI 提出的批注操作".into();
            cx.notify();
            return;
        }
        let draft = input.read(cx).text().to_owned();
        let context = focus_chat_reading_context(reader);
        let result = {
            let chat = reader
                .focus_chat
                .as_mut()
                .expect("visible focus chat remains installed");
            chat.session.set_draft(draft);
            chat.session.submit(context, "简体中文")
        };
        match result {
            Ok(submission) => {
                let request_id = submission.request_id;
                let session_id = submission.session_id;
                let source = Arc::clone(reader.document_sources.presented_source());
                let current_section = submission.current.section_index;
                let book_id = reader.book_id.clone();
                let annotations = reader.highlights.clone();
                let selection = submission.selection.clone();
                input.update(cx, |input, cx| input.set_text("", cx));
                cx.spawn(
                    async move |this: gpui::WeakEntity<Shelf>, cx: &mut gpui::AsyncApp| {
                        let result = cx
                            .background_executor()
                            .spawn(async move {
                                AssistantRuntime::load_default().and_then(|runtime| {
                                    let mut host = DocumentAssistantToolHost::new(
                                        source,
                                        current_section,
                                        book_id,
                                        selection,
                                        annotations,
                                    );
                                    let reply = runtime.complete_with_tool_host_blocking(
                                        &submission,
                                        &mut host,
                                        FOCUS_CHAT_TOOL_STEPS,
                                    )?;
                                    Ok((reply, host.into_pending_annotation_actions()))
                                })
                            })
                            .await;
                        let _ = this.update(cx, |this, cx| {
                            let Some(chat) = this
                                .reader
                                .as_mut()
                                .and_then(|reader| reader.focus_chat.as_mut())
                                .filter(|chat| chat.session.session_id() == session_id)
                            else {
                                return;
                            };
                            match result {
                                Ok((reply, pending)) => {
                                    if chat.session.complete(request_id, reply) {
                                        chat.pending_annotation_actions = pending;
                                        chat.mutation_error = None;
                                    }
                                }
                                Err(error) => {
                                    chat.session.fail(request_id, error);
                                }
                            }
                            cx.notify();
                        });
                    },
                )
                .detach();
            }
            Err(ChatSubmitError::Empty) => reader.status = "请输入问题".into(),
            Err(ChatSubmitError::Busy) => reader.status = "AI 正在回答，请稍候".into(),
        }
        cx.notify();
    }

    fn confirm_focus_chat_mutations(
        &mut self,
        _: &ConfirmFocusChatMutations,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(chat) = reader.focus_chat.as_mut() else {
            return;
        };
        let mut pending = std::mem::take(&mut chat.pending_annotation_actions);
        let result = reader.confirm_pending_annotation_actions(&mut pending);
        let chat = reader
            .focus_chat
            .as_mut()
            .expect("focus chat remains installed during confirmation");
        match result {
            Ok(()) => {
                chat.mutation_error = None;
                reader.status = "AI 批注操作已应用".into();
            }
            Err(error) => {
                chat.pending_annotation_actions = pending;
                chat.mutation_error = Some(format!("应用 AI 批注操作失败：{error}"));
            }
        }
        cx.notify();
    }

    fn cancel_focus_chat_mutations(
        &mut self,
        _: &CancelFocusChatMutations,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(chat) = reader.focus_chat.as_mut() else {
            return;
        };
        let discarded = chat.pending_annotation_actions.len();
        chat.pending_annotation_actions.cancel();
        chat.mutation_error = None;
        reader.status = format!("已取消 {discarded} 项 AI 批注操作").into();
        cx.notify();
    }

    fn cancel_focus_chat(
        &mut self,
        _: &CancelFocusChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(reader) = self.reader.as_mut()
            && let Some(chat) = reader.focus_chat.as_mut()
        {
            chat.visible = false;
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn save_focus_highlight(
        &mut self,
        _: &SaveFocusHighlight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader_settings.is_some() {
            return;
        }
        let result = {
            let Some(reader) = self.reader.as_mut() else {
                return;
            };
            if !reader.focus_state.actions_visible() {
                return;
            }
            reader.focus_state.hide_actions();
            reader.install_active_focus_selection()
        };
        match result {
            Ok(()) => self.save_highlight(&SaveHighlight, window, cx),
            Err(error) => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.status = error.into();
                }
                cx.notify();
            }
        }
    }

    fn open_focus_annotation(
        &mut self,
        _: &OpenFocusAnnotation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader_settings.is_some() {
            return;
        }
        let result = {
            let Some(reader) = self.reader.as_mut() else {
                return;
            };
            if !reader.focus_state.actions_visible() {
                return;
            }
            reader.focus_state.hide_actions();
            reader.install_active_focus_selection()
        };
        match result {
            Ok(()) => self.open_annotation(&OpenAnnotation, window, cx),
            Err(error) => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.status = error.into();
                }
                cx.notify();
            }
        }
    }

    fn select_reader_word(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader_settings.is_some() {
            return;
        }
        window.focus(&self.focus_handle);
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        reader.focus_state.hide_actions();
        reader.focus_state.hide_footnotes();
        if let Some(chat) = reader.focus_chat.as_mut() {
            chat.visible = false;
        }
        let Some(hit) = reader.hit_test_scroll(
            f32::from(event.position.x),
            f32::from(event.position.y),
            true,
        ) else {
            reader.clear_selection();
            reader.status = "未命中文本".into();
            cx.notify();
            return;
        };
        reader.drag_anchor = Some(hit.clone());
        reader.dragging = true;
        if event.modifiers.shift
            && let Some(anchor) = reader.selection_anchor
        {
            let (cluster_start, cluster_end) = hit.cluster_cursors();
            let focus = if hit.cursor() < anchor {
                cluster_start
            } else {
                cluster_end
            };
            match reader.session.selection_between_cursors(anchor, focus) {
                Ok(Some(selection)) => {
                    let count = selection.text.chars().count();
                    reader.selection = Some(selection);
                    reader.selection_anchor = Some(anchor);
                    reader.selection_focus = Some(focus);
                    reader.status = format!("已选择 {count} 个字符").into();
                }
                Ok(None) => {
                    reader.selection = None;
                    reader.selection_anchor = Some(anchor);
                    reader.selection_focus = Some(focus);
                    reader.status = "选择已折叠为光标".into();
                }
                Err(error) => {
                    reader.clear_selection();
                    reader.status = format!("选择失败：{error}").into();
                }
            }
            cx.notify();
            return;
        }
        let granularity = reader.pointer_selection_granularity();
        let selection = reader
            .session
            .selection_between_with_granularity(&hit, &hit, granularity);
        match selection {
            Ok(Some(selection)) => {
                let count = selection.text.chars().count();
                match reader.install_selection(selection) {
                    Ok(()) => reader.status = format!("已选择 {count} 个字符").into(),
                    Err(error) => reader.status = format!("选择失败：{error}").into(),
                }
            }
            Ok(None) => {
                reader.clear_selection();
                reader.status = "未命中可选择文本".into();
            }
            Err(error) => {
                reader.clear_selection();
                reader.status = format!("选择失败：{error}").into();
            }
        }
        cx.notify();
    }

    fn drag_reader_selection(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        if !reader.dragging {
            return;
        }
        let Some(anchor) = reader.drag_anchor.clone() else {
            return;
        };
        let Some(focus) = reader.hit_test_scroll(
            f32::from(event.position.x),
            f32::from(event.position.y),
            false,
        ) else {
            return;
        };
        let granularity = reader.pointer_selection_granularity();
        match reader
            .session
            .selection_between_with_granularity(&anchor, &focus, granularity)
        {
            Ok(Some(selection)) => {
                let count = selection.text.chars().count();
                match reader.install_selection(selection) {
                    Ok(()) => reader.status = format!("已选择 {count} 个字符").into(),
                    Err(error) => reader.status = format!("拖动选择失败：{error}").into(),
                }
            }
            Ok(None) => {}
            Err(error) => reader.status = format!("拖动选择失败：{error}").into(),
        }
        cx.notify();
    }

    fn finish_reader_selection(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        reader.dragging = false;
        reader.drag_anchor = None;
    }

    fn extend_reader_selection(&mut self, direction: TextCursorDirection, cx: &mut Context<Self>) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(current) = reader.selection_focus else {
            return;
        };
        let anchor = reader.selection_anchor.unwrap_or(current);
        let Some(target) = reader.scroll_layout.move_text_cursor(current, direction) else {
            return;
        };
        match reader.session.selection_between_cursors(anchor, target) {
            Ok(Some(selection)) => {
                let count = selection.text.chars().count();
                reader.selection = Some(selection);
                reader.selection_anchor = Some(anchor);
                reader.selection_focus = Some(target);
                reader.status = format!("已选择 {count} 个字符").into();
            }
            Ok(None) => {
                reader.selection = None;
                reader.selection_anchor = Some(anchor);
                reader.selection_focus = Some(target);
                reader.status = "选择已折叠为光标".into();
            }
            Err(error) => reader.status = format!("扩展选择失败：{error}").into(),
        }
        cx.notify();
    }

    fn extend_selection_left(
        &mut self,
        _: &ExtendSelectionLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.extend_reader_selection(TextCursorDirection::Previous, cx);
    }

    fn extend_selection_right(
        &mut self,
        _: &ExtendSelectionRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.extend_reader_selection(TextCursorDirection::Next, cx);
    }

    fn select_current_page(
        &mut self,
        _: &SelectCurrentPage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        match reader.scroll_layout.cursor_range() {
            Some((anchor, focus)) => {
                match reader.session.selection_between_cursors(anchor, focus) {
                    Ok(Some(selection)) => {
                        let count = selection.text.chars().count();
                        reader.selection = Some(selection);
                        reader.selection_anchor = Some(anchor);
                        reader.selection_focus = Some(focus);
                        reader.status = format!("已选择当前阅读单元 {count} 个字符").into();
                    }
                    Ok(None) => reader.clear_selection(),
                    Err(error) => reader.status = format!("全选失败：{error}").into(),
                }
            }
            None => reader.clear_selection(),
        }
        cx.notify();
    }

    fn save_highlight(&mut self, _: &SaveHighlight, _: &mut Window, cx: &mut Context<Self>) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(selection) = reader.selection.as_ref() else {
            reader.status = "请先选择文字".into();
            cx.notify();
            return;
        };
        if reader
            .highlights
            .iter()
            .any(|highlight| highlight.ranges == selection.ranges)
        {
            reader.status = "这段文字已经高亮".into();
            cx.notify();
            return;
        }
        let highlight = StoredHighlight::with_note(
            reader.book_id.clone(),
            selection.ranges.clone(),
            selection.text.clone(),
            None,
        );
        match reader.apply_annotation_actions(&[AssistantAnnotationAction::Create(highlight)]) {
            Ok(()) => {
                reader.status = format!("高亮已保存 · 共 {} 条", reader.highlights.len()).into();
            }
            Err(error) => reader.status = format!("保存高亮失败：{error}").into(),
        }
        cx.notify();
    }

    fn install_annotation_editor(
        &mut self,
        highlight_id: Option<String>,
        ranges: Vec<SourceRange>,
        quote: String,
        note: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| NoteInput::new(cx.focus_handle(), note, "写下这一刻的想法"));
        let focus_handle = input.read(cx).focus_handle();
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        reader.annotation_editor = Some(AnnotationEditor {
            highlight_id,
            ranges,
            quote,
            input,
        });
        window.focus(&focus_handle);
        cx.notify();
    }

    fn open_annotation(&mut self, _: &OpenAnnotation, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_ref() else {
            return;
        };
        let Some(selection) = reader.selection.as_ref() else {
            if let Some(reader) = self.reader.as_mut() {
                reader.status = "请先选择文字".into();
            }
            cx.notify();
            return;
        };
        let existing = reader
            .highlights
            .iter()
            .find(|highlight| highlight.ranges == selection.ranges);
        let highlight_id = existing.map(|highlight| highlight.id.clone());
        let note = existing
            .and_then(|highlight| highlight.note.clone())
            .unwrap_or_default();
        self.install_annotation_editor(
            highlight_id,
            selection.ranges.clone(),
            selection.text.clone(),
            note,
            window,
            cx,
        );
    }

    fn edit_annotation(&mut self, highlight_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let highlight = self.reader.as_ref().and_then(|reader| {
            reader
                .highlights
                .iter()
                .find(|highlight| highlight.id == highlight_id)
                .cloned()
        });
        let Some(highlight) = highlight else {
            return;
        };
        self.install_annotation_editor(
            Some(highlight.id),
            highlight.ranges,
            highlight.quote,
            highlight.note.unwrap_or_default(),
            window,
            cx,
        );
    }

    fn save_annotation(&mut self, _: &SaveAnnotation, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(editor) = reader.annotation_editor.as_ref() else {
            return;
        };
        let note = editor.input.read(cx).text().trim().to_owned();
        if note.is_empty() {
            reader.status = "批注内容不能为空".into();
            cx.notify();
            return;
        }
        let highlight_id = editor.highlight_id.clone();
        let ranges = editor.ranges.clone();
        let quote = editor.quote.clone();
        let result = if let Some(highlight_id) = highlight_id {
            let Some(index) = reader
                .highlights
                .iter()
                .position(|highlight| highlight.id == highlight_id)
            else {
                reader.status = "批注已不存在".into();
                cx.notify();
                return;
            };
            let mut updated = reader.highlights[index].clone();
            updated.note = Some(note);
            reader
                .apply_annotation_actions(&[AssistantAnnotationAction::Update(updated)])
                .map(|()| "批注已更新")
        } else {
            let highlight =
                StoredHighlight::with_note(reader.book_id.clone(), ranges, quote, Some(note));
            reader
                .apply_annotation_actions(&[AssistantAnnotationAction::Create(highlight)])
                .map(|()| "批注已保存")
        };
        match result {
            Ok(message) => {
                reader.annotation_editor = None;
                reader.status = format!("{message} · 共 {} 条标注", reader.highlights.len()).into();
                window.focus(&self.focus_handle);
            }
            Err(error) => reader.status = format!("保存批注失败：{error}").into(),
        }
        cx.notify();
    }

    fn cancel_annotation(
        &mut self,
        _: &CancelAnnotation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        reader.annotation_editor = None;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn go_to_annotation(&mut self, highlight_id: &str, cx: &mut Context<Self>) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(anchor) = reader
            .highlights
            .iter()
            .find(|highlight| highlight.id == highlight_id)
            .and_then(|highlight| highlight.ranges.first())
            .map(|range| range.start.clone())
        else {
            return;
        };
        match reader.session.go_to_source(&anchor) {
            Ok(result) => {
                let location = result.snapshot.location;
                reader.status = format!(
                    "已定位标注 · 第 {} 节 · 第 {}/{} 页",
                    location.section_index + 1,
                    location.page_index + 1,
                    location.page_count
                )
                .into();
                reader.clear_selection();
                if let Err(error) = reader.rebuild_scroll_layout(Some(&anchor)) {
                    reader.status = format!("{} · 连续布局失败：{error}", reader.status).into();
                }
                reader.progress_dirty = true;
                if let Err(error) = reader.persist_progress() {
                    reader.status = format!("{} · 保存进度失败：{error}", reader.status).into();
                }
            }
            Err(error) => reader.status = format!("标注定位失败：{error}").into(),
        }
        cx.notify();
    }

    fn remove_annotation(&mut self, highlight_id: &str, cx: &mut Context<Self>) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        match reader.apply_annotation_actions(&[AssistantAnnotationAction::Delete {
            annotation_id: highlight_id.to_owned(),
        }]) {
            Ok(()) => {
                if reader
                    .annotation_editor
                    .as_ref()
                    .and_then(|editor| editor.highlight_id.as_deref())
                    == Some(highlight_id)
                {
                    reader.annotation_editor = None;
                }
                reader.status = format!("标注已删除 · 剩余 {} 条", reader.highlights.len()).into();
            }
            Err(error) => reader.status = format!("删除标注失败：{error}").into(),
        }
        cx.notify();
    }

    fn copy_selection(&mut self, _: &CopySelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(selection) = reader.selection.as_ref() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(selection.text.clone()));
        reader.status = format!("已复制 {} 个字符", selection.text.chars().count()).into();
        cx.notify();
    }

    fn toggle_contents(&mut self, _: &ToggleContents, _: &mut Window, cx: &mut Context<Self>) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        reader.sidebar = if reader.sidebar == ReaderSidebar::Contents {
            ReaderSidebar::None
        } else {
            ReaderSidebar::Contents
        };
        cx.notify();
    }

    fn toggle_annotations(
        &mut self,
        _: &ToggleAnnotations,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reader_settings.is_some() {
            return;
        }
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        reader.sidebar = if reader.sidebar == ReaderSidebar::Annotations {
            ReaderSidebar::None
        } else {
            ReaderSidebar::Annotations
        };
        cx.notify();
    }

    fn navigate_to_toc(&mut self, item: &TocViewItem, cx: &mut Context<Self>) {
        let Some(target) = item.target.as_ref() else {
            return;
        };
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        match reader.session.go_to_href(target) {
            Ok(result) => {
                let location = result.snapshot.location;
                reader.status = format!(
                    "已跳转至《{}》 · 第 {} 节 · 第 {}/{} 页",
                    item.label,
                    location.section_index + 1,
                    location.page_index + 1,
                    location.page_count
                )
                .into();
                reader.clear_selection();
                let anchor = reader.session.current_reading_unit_anchor();
                if let Err(error) = reader.rebuild_scroll_layout(anchor.as_ref()) {
                    reader.status = format!("{} · 连续布局失败：{error}", reader.status).into();
                }
                reader.progress_dirty = true;
                if let Err(error) = reader.persist_progress() {
                    reader.status = format!("{} · 保存进度失败：{error}", reader.status).into();
                }
            }
            Err(error) => reader.status = format!("目录跳转失败：{error}").into(),
        }
        cx.notify();
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "TOC depth is bounded by the publication tree"
    )]
    fn toc_row(
        index: usize,
        item: &TocViewItem,
        active: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let navigable = item.target.is_some();
        let selected = active == Some(item.id.as_str());
        let owned = item.clone();
        div()
            .id(("reader-toc", index))
            .w_full()
            .min_w_0()
            .pl(px(12.0 + item.depth as f32 * 16.0))
            .pr_3()
            .py_2()
            .rounded_md()
            .text_sm()
            .text_color(rgb(if navigable { 0x0036_403B } else { 0x0090_9894 }))
            .when(selected, |element| element.bg(rgb(0x00DF_EEE6)))
            .when(navigable, |element| {
                element
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x00ED_F4F0)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.navigate_to_toc(&owned, cx);
                        }),
                    )
            })
            .child(item.label.clone())
            .into_any_element()
    }

    fn annotation_row(
        index: usize,
        highlight: &StoredHighlight,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let navigation_id = highlight.id.clone();
        let edit_id = highlight.id.clone();
        let remove_id = highlight.id.clone();
        let quote = highlight.quote.chars().take(72).collect::<String>();
        let note = highlight.note.as_ref().map(|note| {
            let preview = note.chars().take(64).collect::<String>();
            div().text_sm().text_color(rgb(0x0036_403B)).child(preview)
        });
        div()
            .id(("reader-annotation", index))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(rgb(0x00E1_E5E2))
            .child(
                div()
                    .id(("reader-annotation-quote", index))
                    .w_full()
                    .min_w_0()
                    .text_sm()
                    .text_color(rgb(0x0065_706A))
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(0x002F_7253)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.go_to_annotation(&navigation_id, cx);
                        }),
                    )
                    .child(quote),
            )
            .when_some(note, gpui::ParentElement::child)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("reader-annotation-edit", index))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(0x00ED_F4F0)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                    this.edit_annotation(&edit_id, window, cx);
                                }),
                            )
                            .child(if highlight.note.is_some() {
                                "编辑"
                            } else {
                                "添加批注"
                            }),
                    )
                    .child(
                        div()
                            .id(("reader-annotation-remove", index))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(rgb(0x00A0_3D3D))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(0x00FA_ECEC)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.remove_annotation(&remove_id, cx);
                                }),
                            )
                            .child("删除"),
                    ),
            )
            .into_any_element()
    }

    fn book_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let book = &self.books[index];
        let path = book.path.clone();
        let title = book.title.to_string();
        let publication_id = book.publication_id.clone();
        let cover = book.cover.clone();
        let cover_element = div()
            .w(px(68.0))
            .h(px(92.0))
            .flex_none()
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(0x00D8_E5DE))
            .when_some(cover, |element, image| {
                element.child(img(image).size_full().object_fit(ObjectFit::Cover))
            });
        div()
            .id(("shelf-book", book.element_id))
            .flex()
            .gap_4()
            .items_center()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x00D8_DDD9))
            .bg(rgb(0x00FF_FFFD))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x00F0_F6F2)).border_color(rgb(0x0095_B6A4)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.open_book(&path, &publication_id, &title, window, cx);
                }),
            )
            .child(cover_element)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .text_color(rgb(0x001F_2924))
                            .child(book.title.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x0065_706A))
                            .child(book.byline.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x0090_9894))
                            .child(book.file_name.clone()),
                    ),
            )
            .into_any_element()
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::too_many_lines,
        reason = "the first GPUI reader surface keeps one declarative composition and bounded viewport conversion together"
    )]
    fn reader_element(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let settings_open = self.reader_settings.is_some();
        let sidebar = self
            .reader
            .as_ref()
            .map_or(ReaderSidebar::None, |reader| reader.sidebar);
        let toc_open = sidebar == ReaderSidebar::Contents
            && self
                .reader
                .as_ref()
                .is_some_and(|reader| !reader.toc_items.is_empty());
        let annotations_open = sidebar == ReaderSidebar::Annotations;
        let reserved_width = if toc_open {
            READER_TOC_WIDTH
        } else if annotations_open {
            READER_ANNOTATIONS_WIDTH
        } else {
            0.0
        };
        let viewport = reader_viewport(window, reserved_width);
        let (
            title,
            status,
            location,
            page_elements,
            toc_items,
            active_toc_id,
            can_highlight,
            highlights,
            annotation_input,
            focus_footnote_data,
            focus_action_data,
            focus_chat_data,
        ) = {
            let reader = self
                .reader
                .as_mut()
                .expect("reader element requires an active reader");
            reader.sync_visible_scroll_position();
            if viewport != reader.viewport {
                let persisted_ranges = reader
                    .selection
                    .as_ref()
                    .map(|selection| selection.ranges.clone());
                let scroll_anchor = reader
                    .session
                    .current_locator()
                    .source
                    .map(|range| range.start);
                match reader.session.resize(viewport) {
                    Ok(snapshot) => {
                        reader.viewport = viewport;
                        reader.drag_anchor = None;
                        reader.dragging = false;
                        let layout_result = reader.rebuild_scroll_layout(scroll_anchor.as_ref());
                        if layout_result.is_ok() {
                            reader.restore_selection_ranges(persisted_ranges);
                        }
                        reader.status = layout_result
                            .map_or_else(
                                |error| format!("窗口重排后连续布局失败：{error}"),
                                |()| {
                                    format!(
                                        "窗口重排完成 · 第 {}/{} 页",
                                        snapshot.location.page_index + 1,
                                        snapshot.location.page_count
                                    )
                                },
                            )
                            .into();
                    }
                    Err(error) => reader.status = format!("窗口重排失败：{error}").into(),
                }
            }
            reader.sync_visible_scroll_position();
            let snapshot = reader.session.snapshot();
            let location = snapshot.location;
            let active_ranges = reader
                .selection
                .as_ref()
                .map_or(&[][..], |selection| selection.ranges.as_slice());
            let highlight_ranges = reader
                .highlights
                .iter()
                .flat_map(|highlight| highlight.ranges.iter().cloned())
                .collect::<Vec<_>>();
            let focus_ranges = if reader.reading_mode == ReadingMode::Focus {
                reader
                    .focus_units
                    .get(reader.focus_state.active_index())
                    .map_or(&[][..], |unit| unit.paint_ranges.as_slice())
            } else {
                &[]
            };
            let overlays = [
                GpuiSourceOverlay::new(&highlight_ranges, 0xF2D6_6D70),
                GpuiSourceOverlay::new(focus_ranges, 0xB8DC_C866),
                GpuiSourceOverlay::new(active_ranges, 0xB8DC_C866),
            ];
            let scroll_frames = reader.scroll_layout.frames().to_vec();
            let page_elements = scroll_frames
                .iter()
                .enumerate()
                .map(|(index, frame)| {
                    let frame_width = frame.frame.viewport.width as f32;
                    let frame_height = frame.frame.viewport.height as f32;
                    let elements = reader
                        .presenter
                        .elements_with_overlays(&frame.frame, &overlays);
                    div()
                        .id(("reader-frame", index))
                        .relative()
                        .w(px(frame_width))
                        .h(px(frame.height.max(1.0)))
                        .flex_none()
                        .overflow_hidden()
                        .bg(rgb(0x00FF_FCF7))
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::select_reader_word))
                        .on_mouse_move(cx.listener(Self::drag_reader_selection))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(Self::finish_reader_selection),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(px(0.0))
                                .top(px(-frame.origin_y))
                                .w(px(frame_width))
                                .h(px(frame_height))
                                .children(elements),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            let focus_footnote_data = reader.focus_state.footnotes_visible().then(|| {
                let footnotes = reader
                    .focus_units
                    .get(reader.focus_state.active_index())
                    .map(|unit| {
                        unit.footnotes
                            .iter()
                            .map(|footnote| {
                                footnote
                                    .text
                                    .clone()
                                    .unwrap_or_else(|| "未能读取脚注内容".to_owned())
                                    .into()
                            })
                            .collect::<Vec<SharedString>>()
                    })
                    .unwrap_or_default();
                (footnotes, reader.active_focus_window_center_y())
            });
            let focus_action_data = reader.focus_state.actions_visible().then(|| {
                (
                    reader.active_focus_window_center_y(),
                    reader
                        .focus_units
                        .get(reader.focus_state.active_index())
                        .is_some_and(|unit| !unit.is_image()),
                )
            });
            let focus_chat_data = reader.focus_chat.as_ref().map(|chat| {
                let turns = chat
                    .session
                    .turns()
                    .iter()
                    .map(|turn| {
                        (
                            turn.role,
                            turn.display_content
                                .as_deref()
                                .unwrap_or(&turn.content)
                                .to_owned()
                                .into(),
                        )
                    })
                    .collect::<Vec<(ChatRole, SharedString)>>();
                let stream: Option<SharedString> = match chat.session.activity() {
                    ChatActivity::Idle => None,
                    ChatActivity::Pending { .. } => Some("思考中…".into()),
                    ChatActivity::Streaming { content, .. } => Some(content.clone().into()),
                };
                let pending_count = chat.pending_annotation_actions.len();
                let pending_summaries = chat
                    .pending_annotation_actions
                    .actions()
                    .iter()
                    .take(3)
                    .map(focus_chat_annotation_action_summary)
                    .collect::<Vec<_>>();
                (
                    chat.visible,
                    chat.input.clone(),
                    turns,
                    stream,
                    chat.session
                        .error()
                        .map(|error| SharedString::from(error.to_owned())),
                    chat.mutation_error
                        .as_ref()
                        .map(|error| SharedString::from(error.clone())),
                    pending_count,
                    pending_summaries,
                    reader.active_focus_window_center_y(),
                )
            });
            (
                reader.title.clone(),
                reader.status.clone(),
                location,
                page_elements,
                Arc::clone(&reader.toc_items),
                snapshot.active_toc_id,
                reader.selection.is_some(),
                reader.highlights.clone(),
                reader
                    .annotation_editor
                    .as_ref()
                    .map(|editor| editor.input.clone()),
                focus_footnote_data,
                focus_action_data,
                focus_chat_data,
            )
        };
        let fixed_page = self.reader.as_ref().is_some_and(|reader| {
            reader.session.book_source().book().metadata.layout == RenditionLayout::PrePaginated
        });
        let reader_settings_data = self.reader_settings.as_ref().map(|editor| {
            (
                editor.draft.clone(),
                editor.error.clone().map(SharedString::from),
            )
        });

        let previous = div()
            .id("reader-previous")
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x00E7_EDEA)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.turn_page(PageDirection::Previous, cx);
                }),
            )
            .child("上一节");
        let next = div()
            .id("reader-next")
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x00E7_EDEA)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.turn_page(PageDirection::Next, cx);
                }),
            )
            .child("下一节");
        let highlight = div()
            .id("reader-highlight")
            .px_3()
            .py_2()
            .rounded_md()
            .text_color(rgb(if can_highlight {
                0x0036_403B
            } else {
                0x0090_9894
            }))
            .when(can_highlight, |element| {
                element
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x00F7_EFC9)))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.save_highlight(&SaveHighlight, window, cx);
                }),
            )
            .child("高亮");
        let annotate = div()
            .id("reader-annotate")
            .px_3()
            .py_2()
            .rounded_md()
            .text_color(rgb(if can_highlight {
                0x0036_403B
            } else {
                0x0090_9894
            }))
            .when(can_highlight, |element| {
                element
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x00E7_EDEA)))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.open_annotation(&OpenAnnotation, window, cx);
                }),
            )
            .child("批注");
        let toc_rows = toc_items
            .iter()
            .enumerate()
            .map(|(index, item)| Self::toc_row(index, item, active_toc_id.as_deref(), cx))
            .collect::<Vec<_>>();
        let toc_panel = div()
            .w(px(READER_TOC_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(0x00D1_D7D3))
            .bg(rgb(0x00FF_FFFD))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(0x00E1_E5E2))
                    .text_sm()
                    .text_color(rgb(0x0065_706A))
                    .child("目录"),
            )
            .child(
                div()
                    .id("gpui-reader-toc-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_2()
                    .children(toc_rows),
            );
        let annotation_rows = highlights
            .iter()
            .enumerate()
            .map(|(index, highlight)| Self::annotation_row(index, highlight, cx))
            .collect::<Vec<_>>();
        let annotation_panel = div()
            .w(px(READER_ANNOTATIONS_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(0x00D1_D7D3))
            .bg(rgb(0x00FF_FFFD))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(0x00E1_E5E2))
                    .text_sm()
                    .text_color(rgb(0x0065_706A))
                    .child(format!("标注 · {}", highlights.len())),
            )
            .child(
                div()
                    .id("gpui-reader-annotation-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .when(annotation_rows.is_empty(), |element| {
                        element.child(
                            div()
                                .p_4()
                                .text_sm()
                                .text_color(rgb(0x0090_9894))
                                .child("选择正文后可以添加高亮或批注"),
                        )
                    })
                    .children(annotation_rows),
            );
        let annotation_editor = annotation_input.map(|input| {
            let can_save = !input.read(cx).text().trim().is_empty();
            div()
                .absolute()
                .top(px(82.0))
                .right(px(24.0))
                .w(px(380.0))
                .p_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(0x00D1_D7D3))
                .bg(rgb(0x00FF_FFFD))
                .shadow_lg()
                .child(div().min_w_0().flex_1().child(input))
                .child(
                    div()
                        .id("reader-annotation-save")
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .text_sm()
                        .text_color(rgb(if can_save { 0x00FF_FFFF } else { 0x0090_9894 }))
                        .bg(rgb(if can_save { 0x0044_8C6A } else { 0x00E7_EDEA }))
                        .when(can_save, |element| {
                            element
                                .cursor_pointer()
                                .hover(|style| style.bg(rgb(0x0038_7659)))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                this.save_annotation(&SaveAnnotation, window, cx);
                            }),
                        )
                        .child("保存"),
                )
        });
        let focus_chat_overlay = focus_chat_data.and_then(
            |(
                visible,
                input,
                turns,
                stream,
                error,
                mutation_error,
                pending_count,
                pending_summaries,
                center_y,
            )| {
                if !visible {
                    return None;
                }
                let center_y = center_y?;
                let window_width = f32::from(window.bounds().size.width);
                let window_height = f32::from(window.bounds().size.height);
                let page_width =
                    u16::try_from(viewport.width).map_or(f32::from(u16::MAX), f32::from);
                let reader_area_width = (window_width - reserved_width).max(page_width);
                let page_left = reserved_width + (reader_area_width - page_width) * 0.5;
                let width = 360.0_f32.min((window_width - 48.0).max(240.0));
                let x = (page_left + page_width + 12.0).min(window_width - width - 24.0);
                let y = (center_y - 150.0).clamp(96.0, (window_height - 400.0).max(96.0));
                let mut rows = turns
                    .into_iter()
                    .enumerate()
                    .map(|(index, (role, content))| {
                        div()
                            .id(("focus-chat-turn", index))
                            .w_full()
                            .flex()
                            .when(role == ChatRole::User, gpui::Styled::justify_end)
                            .child(
                                div()
                                    .max_w(px(width - 48.0))
                                    .px_3()
                                    .py_2()
                                    .rounded_lg()
                                    .text_sm()
                                    .text_color(rgb(0x0036_403B))
                                    .bg(rgb(if role == ChatRole::User {
                                        0x00E7_EDEA
                                    } else {
                                        0x00FF_FFFD
                                    }))
                                    .child(content),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>();
                if let Some(stream) = stream {
                    rows.push(
                        div()
                            .w_full()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(rgb(0x0065_706A))
                            .child(stream)
                            .into_any_element(),
                    );
                }
                if let Some(error) = error {
                    rows.push(
                        div()
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(0x00FA_EAE7))
                            .text_sm()
                            .text_color(rgb(0x00A8_3B32))
                            .child(error)
                            .into_any_element(),
                    );
                }
                if let Some(error) = mutation_error {
                    rows.push(
                        div()
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(0x00FA_EAE7))
                            .text_sm()
                            .text_color(rgb(0x00A8_3B32))
                            .child(error)
                            .into_any_element(),
                    );
                }
                if pending_count > 0 {
                    let summaries = pending_summaries
                        .into_iter()
                        .enumerate()
                        .map(|(index, summary)| {
                            div()
                                .id(("focus-chat-mutation-summary", index))
                                .text_xs()
                                .text_color(rgb(0x0065_706A))
                                .child(summary)
                        })
                        .collect::<Vec<_>>();
                    rows.push(
                        div()
                            .id("focus-chat-mutation-confirmation")
                            .w_full()
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0x00D1_D7D3))
                            .bg(rgb(0x00F7_F8F7))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x0036_403B))
                                    .child(format!("AI 请求执行 {pending_count} 项批注操作")),
                            )
                            .children(summaries)
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("focus-chat-mutation-cancel")
                                            .px_3()
                                            .py_1()
                                            .rounded_md()
                                            .cursor_pointer()
                                            .text_sm()
                                            .text_color(rgb(0x0065_706A))
                                            .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    |this, _: &MouseDownEvent, window, cx| {
                                                        this.cancel_focus_chat_mutations(
                                                            &CancelFocusChatMutations,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                ),
                                            )
                                            .child("取消"),
                                    )
                                    .child(
                                        div()
                                            .id("focus-chat-mutation-confirm")
                                            .px_3()
                                            .py_1()
                                            .rounded_md()
                                            .cursor_pointer()
                                            .text_sm()
                                            .text_color(rgb(0x00FF_FFFF))
                                            .bg(rgb(0x0044_8C6A))
                                            .hover(|style| style.bg(rgb(0x0038_7659)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    |this, _: &MouseDownEvent, window, cx| {
                                                        this.confirm_focus_chat_mutations(
                                                            &ConfirmFocusChatMutations,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                ),
                                            )
                                            .child("确认"),
                                    ),
                            )
                            .into_any_element(),
                    );
                }
                let can_send = pending_count == 0 && !input.read(cx).text().trim().is_empty();
                Some(
                    div()
                        .id("focus-chat-overlay")
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(width))
                        .max_h(px(380.0))
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0x00D1_D7D3))
                        .bg(rgb(0x00FF_FFFD))
                        .shadow_lg()
                        .child(
                            div()
                                .id("focus-chat-turns")
                                .max_h(px(280.0))
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .when(rows.is_empty(), |element| {
                                    element.child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .text_sm()
                                            .text_color(rgb(0x0090_9894))
                                            .child("围绕当前段落提问"),
                                    )
                                })
                                .children(rows),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().min_w_0().flex_1().child(input))
                                .child(
                                    div()
                                        .id("focus-chat-send")
                                        .w(px(38.0))
                                        .h(px(34.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_md()
                                        .text_color(rgb(if can_send {
                                            0x00FF_FFFF
                                        } else {
                                            0x0090_9894
                                        }))
                                        .bg(rgb(if can_send { 0x0044_8C6A } else { 0x00E7_EDEA }))
                                        .when(can_send, |element| {
                                            element
                                                .cursor_pointer()
                                                .hover(|style| style.bg(rgb(0x0038_7659)))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this, _: &MouseDownEvent, window, cx| {
                                                            this.send_focus_chat(
                                                                &SendFocusChat,
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    ),
                                                )
                                        })
                                        .child("↑"),
                                ),
                        ),
                )
            },
        );
        let focus_action_overlay = focus_action_data.and_then(|(center_y, can_annotate)| {
            let center_y = center_y?;
            let window_width = f32::from(window.bounds().size.width);
            let window_height = f32::from(window.bounds().size.height);
            let page_width = u16::try_from(viewport.width).map_or(f32::from(u16::MAX), f32::from);
            let reader_area_width = (window_width - reserved_width).max(page_width);
            let page_left = reserved_width + (reader_area_width - page_width) * 0.5;
            let x = (page_left + page_width + 12.0).min(window_width - 56.0);
            let y = (center_y - 66.0).clamp(96.0, (window_height - 160.0).max(96.0));
            let button = |id: &'static str, label: &'static str| {
                div()
                    .id(id)
                    .w(px(44.0))
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x00D1_D7D3))
                    .bg(rgb(0x00FF_FFFD))
                    .shadow_sm()
                    .child(label)
            };
            let chat = button("focus-action-chat", "1 · 聊")
                .cursor_pointer()
                .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.open_focus_chat(&OpenFocusChat, window, cx);
                    }),
                );
            let highlight = button("focus-action-highlight", "2 · 亮")
                .text_color(rgb(if can_annotate {
                    0x0036_403B
                } else {
                    0x0090_9894
                }))
                .when(can_annotate, |element| {
                    element
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(0x00F7_EFC9)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                this.save_focus_highlight(&SaveFocusHighlight, window, cx);
                            }),
                        )
                });
            let note = button("focus-action-note", "3 · 注")
                .text_color(rgb(if can_annotate {
                    0x0036_403B
                } else {
                    0x0090_9894
                }))
                .when(can_annotate, |element| {
                    element
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                this.open_focus_annotation(&OpenFocusAnnotation, window, cx);
                            }),
                        )
                });
            Some(
                div()
                    .id("focus-actions-overlay")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children([chat, highlight, note]),
            )
        });
        let focus_footnote_overlay = focus_footnote_data.and_then(|(footnotes, center_y)| {
            let center_y = center_y?;
            if footnotes.is_empty() {
                return None;
            }
            let window_width = f32::from(window.bounds().size.width);
            let window_height = f32::from(window.bounds().size.height);
            let page_width = u16::try_from(viewport.width).map_or(f32::from(u16::MAX), f32::from);
            let reader_area_width = (window_width - reserved_width).max(page_width);
            let page_left = reserved_width + (reader_area_width - page_width) * 0.5;
            let preferred_x = page_left + page_width + 12.0;
            let maximum_width = 360.0_f32.min((window_width - 32.0).max(1.0));
            let minimum_width = 180.0_f32.min(maximum_width);
            let available_right = window_width - preferred_x - 16.0;
            let width = available_right.clamp(minimum_width, maximum_width);
            let x = if available_right >= minimum_width {
                preferred_x
            } else {
                (window_width - width - 16.0).max(16.0)
            };
            let y = (center_y - 72.0).clamp(96.0, (window_height - 200.0).max(96.0));
            let rows = footnotes.into_iter().enumerate().map(|(index, footnote)| {
                div()
                    .id(("focus-footnote", index))
                    .flex()
                    .items_start()
                    .gap_2()
                    .when(index > 0, |element| {
                        element.pt_3().border_t_1().border_color(rgb(0x00E1_E5E2))
                    })
                    .child(div().text_color(rgb(0x0044_8C6A)).child("ⓘ"))
                    .child(div().min_w_0().flex_1().child(footnote))
            });
            Some(
                div()
                    .id("focus-footnote-overlay")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(width))
                    .max_h(px(380.0))
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x00D1_D7D3))
                    .bg(rgb(0x00FF_FFFD))
                    .shadow_lg()
                    .children(rows)
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x0090_9894))
                            .child("Alt 关闭"),
                    ),
            )
        });
        let reader_settings_overlay = reader_settings_data.map(|(draft, error)| {
            let effective = draft.resolve(!fixed_page, fixed_page);
            let focus_mode = effective.presentation.mode == ReadingMode::Focus;
            let mut book_typesetting = draft.typesetting.clone();
            book_typesetting.mode = TypesettingMode::Book;
            let mut unified_typesetting = draft.typesetting.clone();
            unified_typesetting.mode = TypesettingMode::Unified;

            let reading_mode_controls = if fixed_page {
                div()
                    .text_sm()
                    .text_color(rgb(0x0065_706A))
                    .child("固定版式使用经典模式；不会修改全局阅读模式偏好。")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .gap_2()
                    .children([
                        Self::reader_setting_choice(
                            "settings-mode-focus",
                            "专注",
                            draft.reading_mode == ReadingMode::Focus,
                            ReaderDocumentPreferenceChange::ReadingMode(ReadingMode::Focus),
                            cx,
                        ),
                        Self::reader_setting_choice(
                            "settings-mode-classic",
                            "经典",
                            draft.reading_mode == ReadingMode::Classic,
                            ReaderDocumentPreferenceChange::ReadingMode(ReadingMode::Classic),
                            cx,
                        ),
                    ])
                    .into_any_element()
            };
            let spread_controls = (!focus_mode).then(|| {
                div().flex().gap_2().children([
                    Self::reader_setting_choice(
                        "settings-spread-single",
                        "单栏",
                        draft.spread == SpreadMode::Single,
                        ReaderDocumentPreferenceChange::Spread(SpreadMode::Single),
                        cx,
                    ),
                    Self::reader_setting_choice(
                        "settings-spread-double",
                        "双栏",
                        draft.spread == SpreadMode::Double,
                        ReaderDocumentPreferenceChange::Spread(SpreadMode::Double),
                        cx,
                    ),
                    Self::reader_setting_choice(
                        "settings-spread-scroll",
                        "滑动",
                        draft.spread == SpreadMode::Scroll,
                        ReaderDocumentPreferenceChange::Spread(SpreadMode::Scroll),
                        cx,
                    ),
                ])
            });
            let selection_controls = if focus_mode {
                div()
                    .text_sm()
                    .text_color(rgb(0x0065_706A))
                    .child("段落（专注模式固定）")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children([
                        Self::reader_setting_choice(
                            "settings-selection-free",
                            "自由",
                            draft.selection_granularity == SelectionGranularity::Free,
                            ReaderDocumentPreferenceChange::SelectionGranularity(
                                SelectionGranularity::Free,
                            ),
                            cx,
                        ),
                        Self::reader_setting_choice(
                            "settings-selection-word",
                            "单词",
                            draft.selection_granularity == SelectionGranularity::Word,
                            ReaderDocumentPreferenceChange::SelectionGranularity(
                                SelectionGranularity::Word,
                            ),
                            cx,
                        ),
                        Self::reader_setting_choice(
                            "settings-selection-sentence",
                            "句子",
                            draft.selection_granularity == SelectionGranularity::Sentence,
                            ReaderDocumentPreferenceChange::SelectionGranularity(
                                SelectionGranularity::Sentence,
                            ),
                            cx,
                        ),
                        Self::reader_setting_choice(
                            "settings-selection-paragraph",
                            "段落",
                            draft.selection_granularity == SelectionGranularity::Paragraph,
                            ReaderDocumentPreferenceChange::SelectionGranularity(
                                SelectionGranularity::Paragraph,
                            ),
                            cx,
                        ),
                    ])
                    .into_any_element()
            };
            let value_control =
                |label: String, decrease: AnyElement, increase: AnyElement| -> AnyElement {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(decrease)
                        .child(div().w(px(76.0)).text_center().child(label))
                        .child(increase)
                        .into_any_element()
                };
            let font_size_control = value_control(
                format!("{:.0} px", draft.typography.font_size),
                Self::reader_setting_adjustment(
                    "settings-font-decrease",
                    "−",
                    ReaderSettingsAdjustment::FontSize(-1.0),
                    cx,
                ),
                Self::reader_setting_adjustment(
                    "settings-font-increase",
                    "+",
                    ReaderSettingsAdjustment::FontSize(1.0),
                    cx,
                ),
            );
            let line_height_control = value_control(
                format!("{:.1}", draft.typesetting.body_line_height),
                Self::reader_setting_adjustment(
                    "settings-line-height-decrease",
                    "−",
                    ReaderSettingsAdjustment::LineHeight(-0.1),
                    cx,
                ),
                Self::reader_setting_adjustment(
                    "settings-line-height-increase",
                    "+",
                    ReaderSettingsAdjustment::LineHeight(0.1),
                    cx,
                ),
            );
            let row = |label: &'static str, control: AnyElement| -> AnyElement {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .w(px(92.0))
                            .flex_none()
                            .text_sm()
                            .text_color(rgb(0x0065_706A))
                            .child(label),
                    )
                    .child(div().min_w_0().flex_1().child(control))
                    .into_any_element()
            };
            let typesetting_controls = div()
                .flex()
                .gap_2()
                .children([
                    Self::reader_setting_choice(
                        "settings-typesetting-unified",
                        "统一版式",
                        draft.typesetting.mode == TypesettingMode::Unified,
                        ReaderDocumentPreferenceChange::Typesetting(unified_typesetting),
                        cx,
                    ),
                    Self::reader_setting_choice(
                        "settings-typesetting-book",
                        "书籍版式",
                        draft.typesetting.mode == TypesettingMode::Book,
                        ReaderDocumentPreferenceChange::Typesetting(book_typesetting),
                        cx,
                    ),
                ])
                .into_any_element();

            div()
                .id("reader-settings-scrim")
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x2630_2B28))
                .child(
                    div()
                        .id("reader-settings-dialog")
                        .w(px(520.0))
                        .max_h(px(640.0))
                        .overflow_y_scroll()
                        .p_5()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .rounded_xl()
                        .border_1()
                        .border_color(rgb(0x00D1_D7D3))
                        .bg(rgb(0x00FF_FFFD))
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_xl().child("阅读设置"))
                                .child(
                                    div()
                                        .id("reader-settings-close")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                                this.cancel_reader_settings(
                                                    &CancelReaderSettings,
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        )
                                        .child("关闭"),
                                ),
                        )
                        .child(row("阅读模式", reading_mode_controls))
                        .when_some(spread_controls, |element, control| {
                            element.child(row("分页模式", control.into_any_element()))
                        })
                        .child(row("正文大小", font_size_control))
                        .child(row("正文行高", line_height_control))
                        .child(row("版式来源", typesetting_controls))
                        .child(row("文字选择", selection_controls))
                        .when_some(error, |element, error| {
                            element.child(
                                div()
                                    .p_3()
                                    .rounded_md()
                                    .bg(rgb(0x00FA_E9E6))
                                    .text_sm()
                                    .text_color(rgb(0x00B3_4236))
                                    .child(error),
                            )
                        })
                        .child(
                            div()
                                .pt_3()
                                .border_t_1()
                                .border_color(rgb(0x00E1_E5E2))
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id("reader-settings-cancel")
                                        .px_4()
                                        .py_2()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                                this.cancel_reader_settings(
                                                    &CancelReaderSettings,
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        )
                                        .child("取消"),
                                )
                                .child(
                                    div()
                                        .id("reader-settings-save")
                                        .px_4()
                                        .py_2()
                                        .rounded_md()
                                        .bg(rgb(0x0044_8C6A))
                                        .text_color(rgb(0x00FF_FFFF))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgb(0x003A_7C5D)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                                this.save_reader_settings(
                                                    &SaveReaderSettings,
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        )
                                        .child("保存"),
                                ),
                        ),
                )
        });
        let reader_scroll = div()
            .id("gpui-reader-scroll")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .on_scroll_wheel(
                cx.listener(|_: &mut Self, _: &gpui::ScrollWheelEvent, _, cx| {
                    // GPUI can update a tracked `ScrollHandle` without rerendering the
                    // owning entity. Schedule a render after the wheel event so the
                    // source-backed visible position and progress follow the viewport.
                    cx.notify();
                }),
            )
            .track_scroll(
                &self
                    .reader
                    .as_ref()
                    .expect("reader scroll requires an active reader")
                    .scroll,
            )
            .p_4()
            .flex()
            .flex_col()
            .items_center()
            .children(page_elements);

        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::back_to_shelf))
            .on_action(cx.listener(Self::previous_page))
            .on_action(cx.listener(Self::next_page))
            .on_action(cx.listener(Self::previous_focus_unit))
            .on_action(cx.listener(Self::next_focus_unit))
            .on_action(cx.listener(Self::toggle_focus_actions))
            .on_action(cx.listener(Self::open_focus_chat))
            .on_action(cx.listener(Self::send_focus_chat))
            .on_action(cx.listener(Self::cancel_focus_chat))
            .on_action(cx.listener(Self::confirm_focus_chat_mutations))
            .on_action(cx.listener(Self::cancel_focus_chat_mutations))
            .on_action(cx.listener(Self::save_focus_highlight))
            .on_action(cx.listener(Self::open_focus_annotation))
            .on_action(cx.listener(Self::toggle_focus_footnotes))
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::toggle_contents))
            .on_action(cx.listener(Self::toggle_annotations))
            .on_action(cx.listener(Self::extend_selection_left))
            .on_action(cx.listener(Self::extend_selection_right))
            .on_action(cx.listener(Self::select_current_page))
            .on_action(cx.listener(Self::save_highlight))
            .on_action(cx.listener(Self::open_annotation))
            .on_action(cx.listener(Self::save_annotation))
            .on_action(cx.listener(Self::cancel_annotation))
            .on_action(cx.listener(Self::toggle_reader_settings))
            .on_action(cx.listener(Self::save_reader_settings))
            .on_action(cx.listener(Self::cancel_reader_settings))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(0x00E9_ECEA))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_5()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(0x00D1_D7D3))
                    .bg(rgb(0x00FF_FFFD))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("reader-back")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                            this.back_to_shelf(&BackToShelf, window, cx);
                                        }),
                                    )
                                    .child("返回书架"),
                            )
                            .child(
                                div()
                                    .id("reader-contents")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                            this.toggle_contents(&ToggleContents, window, cx);
                                        }),
                                    )
                                    .child(if toc_open { "隐藏目录" } else { "目录" }),
                            )
                            .child(
                                div()
                                    .id("reader-annotations")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                            this.toggle_annotations(&ToggleAnnotations, window, cx);
                                        }),
                                    )
                                    .child(if annotations_open {
                                        "隐藏标注"
                                    } else {
                                        "标注"
                                    }),
                            )
                            .child(
                                div()
                                    .id("reader-settings")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0x00E7_EDEA)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                            this.toggle_reader_settings(
                                                &ToggleReaderSettings,
                                                window,
                                                cx,
                                            );
                                        }),
                                    )
                                    .child(if settings_open {
                                        "关闭设置"
                                    } else {
                                        "设置"
                                    }),
                            ),
                    )
                    .child(div().min_w_0().flex_1().px_4().text_center().child(title))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(highlight)
                            .child(annotate)
                            .child(previous)
                            .child(next),
                    ),
            )
            .child(
                div()
                    .px_5()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(0x0065_706A))
                    .child(format!(
                        "{} · 章节 {} · 页 {}/{}",
                        status,
                        location.section_index + 1,
                        location.page_index + 1,
                        location.page_count
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(toc_open, |element| element.child(toc_panel))
                    .when(annotations_open, |element| element.child(annotation_panel))
                    .child(reader_scroll),
            )
            .when_some(annotation_editor, gpui::ParentElement::child)
            .when_some(focus_chat_overlay, gpui::ParentElement::child)
            .when_some(focus_action_overlay, gpui::ParentElement::child)
            .when_some(focus_footnote_overlay, gpui::ParentElement::child)
            .when_some(reader_settings_overlay, gpui::ParentElement::child)
            .into_any_element()
    }
}

impl Render for Shelf {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.reader.is_some() {
            return self.reader_element(window, cx);
        }
        let book_rows = (0..self.books.len())
            .map(|index| self.book_row(index, cx))
            .collect::<Vec<_>>();
        let content = if self.books.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .text_xl()
                        .text_color(rgb(0x0034_403A))
                        .child("书架还是空的"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x0071_7B76))
                        .child("旧桌面端导入书籍后，按 Ctrl/Cmd+R 即可在这里刷新。"),
                )
                .into_any_element()
        } else {
            div()
                .id("gpui-shelf-scroll")
                .flex_1()
                .overflow_y_scroll()
                .px_8()
                .py_6()
                .child(
                    div()
                        .w_full()
                        .max_w(px(840.0))
                        .mx_auto()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .children(book_rows),
                )
                .into_any_element()
        };

        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::refresh))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x00F4_F3EF))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_8()
                    .py_4()
                    .border_b_1()
                    .border_color(rgb(0x00D1_D7D3))
                    .bg(rgb(0x00FF_FFFD))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_3()
                            .child(div().text_2xl().child("Torto"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x0065_706A))
                                    .child(format!("{} 本书", self.books.len())),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(if self.load_error.is_some() {
                                rgb(0x00B3_4236)
                            } else {
                                rgb(0x0065_706A)
                            })
                            .child(self.status.clone()),
                    ),
            )
            .child(content)
            .into_any_element()
    }
}

fn load_books() -> Result<Vec<ShelfBook>, String> {
    LocalLibrary::load_default()
        .map(|library| {
            library
                .books()
                .iter()
                .map(ShelfBook::from_library)
                .collect()
        })
        .map_err(|error| error.to_string())
}

fn cover_image(bytes: &[u8]) -> Option<Arc<Image>> {
    let format = match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => ImageFormat::Png,
        image::ImageFormat::Jpeg => ImageFormat::Jpeg,
        image::ImageFormat::WebP => ImageFormat::Webp,
        image::ImageFormat::Gif => ImageFormat::Gif,
        image::ImageFormat::Bmp => ImageFormat::Bmp,
        image::ImageFormat::Tiff => ImageFormat::Tiff,
        _ => return None,
    };
    Some(Arc::new(Image::from_bytes(format, bytes.to_vec())))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "window dimensions are clamped to positive, bounded logical pixels"
)]
fn reader_viewport(window: &Window, reserved_width: f32) -> LayoutViewport {
    let bounds = window.bounds().size;
    let width = (f32::from(bounds.width) - READER_OUTER_PADDING * 2.0 - reserved_width)
        .clamp(MIN_READER_WIDTH, MAX_READER_WIDTH);
    let height = (f32::from(bounds.height) - READER_CHROME_HEIGHT - READER_OUTER_PADDING)
        .max(MIN_READER_HEIGHT);
    LayoutViewport::new(width.round() as u32, height.round() as u32)
        .expect("clamped GPUI reader viewport must be valid")
}

fn compile_focus_presentation(
    session: &mut ReaderSession,
    scroll_layout: &ReaderScrollLayout,
    anchor: Option<SourceAnchor>,
    current: ReaderPosition,
) -> Result<(Vec<ReaderFocusUnit>, ReaderFocusState), String> {
    let layout = session
        .current_focus_layout(
            scroll_layout,
            anchor.as_ref(),
            ReaderFocusLayoutPolicy::default(),
            |_| false,
        )
        .map_err(|error| error.to_string())?;
    let mut state = ReaderFocusState::new(anchor);
    state.resolve(&layout.units, layout.first_unit_after_anchor, current);
    Ok((layout.units, state))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "layout viewport heights are bounded logical pixels consumed by f32 frame geometry"
)]
fn focus_scroll_target(
    units: &[ReaderFocusUnit],
    state: &ReaderFocusState,
    viewport: LayoutViewport,
) -> Option<PendingReaderScroll> {
    units.get(state.active_index()).map(|unit| {
        let target = ReaderFocusViewportPolicy::default()
            .unit_viewport_target(unit.geometry.bounds, viewport.height as f32);
        PendingReaderScroll {
            content_y: target.content_y,
            viewport_y: target.viewport_y,
        }
    })
}

fn focus_chat_reading_context(reader: &ReaderSurface) -> ChatReadingContext {
    let snapshot = reader.session.snapshot();
    let location = snapshot.location;
    let active_toc = snapshot
        .active_toc_id
        .as_deref()
        .and_then(|id| reader.toc_items.iter().find(|item| item.id.as_str() == id));
    let toc_label = active_toc.map(|item| item.label.clone());
    let toc_href = active_toc
        .and_then(|item| item.target.as_ref())
        .map(ToString::to_string);
    let book = reader.session.book();
    let fixed_page = book.metadata.layout == RenditionLayout::PrePaginated;
    let spine = book.sections.get(location.section_index);
    let current_title = toc_label
        .clone()
        .unwrap_or_else(|| reader.title.to_string());
    let to_f64 = |value: usize| f64::from(u32::try_from(value).unwrap_or(u32::MAX));
    let page_fraction = if location.page_count <= 1 {
        0.0
    } else {
        to_f64(location.page_index) / to_f64(location.page_count - 1)
    };
    let section_fraction = ((to_f64(location.segment_index) + page_fraction)
        / to_f64(location.segment_count.max(1)))
    .clamp(0.0, 1.0);
    ChatReadingContext {
        unit_index: location.section_index,
        unit_id: spine.map(|item| item.id.as_str().to_owned()),
        unit_kind: if fixed_page { "page" } else { "section" }.into(),
        unit_title: Some(current_title.clone()),
        section_index: location.section_index,
        section_id: if fixed_page {
            None
        } else {
            spine.map(|item| item.id.as_str().to_owned())
        },
        section_title: (!fixed_page).then_some(current_title),
        toc_label,
        toc_href,
        section_fraction,
        total_fraction: snapshot.total_progression,
        segment_index: location.segment_index,
        segment_count: location.segment_count,
        page_index: if fixed_page {
            location.section_index
        } else {
            location.page_index
        },
        page_count: if fixed_page {
            book.sections.len()
        } else {
            location.page_count
        },
    }
}

fn stored_source_anchor(progress: Option<&StoredProgress>) -> Option<SourceAnchor> {
    progress
        .and_then(|progress| progress.locator.source.as_ref())
        .map(|range| range.start.clone())
}

fn open_or_resume_reader<F, E>(
    source: Arc<dyn BookSource>,
    viewport: LayoutViewport,
    style: &ReaderStyle,
    make_text_engine: &F,
    progress: Option<&StoredProgress>,
) -> Result<(ReaderSession, bool, bool), String>
where
    F: Fn() -> E,
    E: TextEngine + 'static,
{
    if let Some(progress) = progress {
        if let Ok(session) = ReaderSession::open_with_text_engine_at_locator(
            Arc::clone(&source),
            viewport,
            style.clone(),
            make_text_engine(),
            &progress.locator,
        ) {
            return Ok((session, true, false));
        }
        let session = ReaderSession::open_with_text_engine(
            source,
            viewport,
            style.clone(),
            make_text_engine(),
        )
        .map_err(|error| error.to_string())?;
        return Ok((session, false, true));
    }
    let session =
        ReaderSession::open_with_text_engine(source, viewport, style.clone(), make_text_engine())
            .map_err(|error| error.to_string())?;
    Ok((session, false, false))
}

fn open_reader_session<F, E>(
    path: &Path,
    publication_id: &str,
    title: &str,
    viewport: LayoutViewport,
    make_text_engine: F,
    progress_store: SyncStore,
    reader_preferences: &ReaderDocumentPreferences,
) -> Result<ReaderSurface, String>
where
    F: Fn() -> E,
    E: TextEngine + 'static,
{
    let publication =
        open_file_for_reading(path, Some(publication_id)).map_err(|error| error.to_string())?;
    let document_sources =
        DocumentSourcePipeline::new(publication.source(), TranslationMode::default());
    let source = Arc::clone(document_sources.presented_source());
    let fixed_page = source.book().metadata.layout == RenditionLayout::PrePaginated;
    let resolved_preferences = reader_preferences.resolve(!fixed_page, fixed_page);
    let stored_progress = progress_store
        .load_progress(publication_id)
        .map_err(|error| error.to_string())?;
    let stored_source_anchor = stored_source_anchor(stored_progress.as_ref());
    let (mut session, resumed, resume_failed) = open_or_resume_reader(
        source,
        viewport,
        &resolved_preferences.style,
        &make_text_engine,
        stored_progress.as_ref(),
    )?;
    let highlight_store = HighlightStore::from_repository(progress_store.clone());
    let highlights = highlight_store.for_book(publication_id);
    let toc_items = session.toc_items().to_vec().into();
    let location = session.location();
    let position = ReaderPosition {
        section_index: location.section_index,
        segment_index: location.segment_index,
        page_index: location.page_index,
    };
    let locator = session.current_locator();
    let scroll_layout = session
        .current_scroll_layout(false)
        .map_err(|error| error.to_string())?;
    let visible_frame_index = scroll_layout.frame_index(position).unwrap_or(0);
    let pending_source_anchor = resumed
        .then_some(stored_source_anchor.as_ref())
        .flatten()
        .or_else(|| locator.source.as_ref().map(|source| &source.start))
        .cloned();
    let (focus_units, focus_state) = compile_focus_presentation(
        &mut session,
        &scroll_layout,
        pending_source_anchor.clone(),
        position,
    )?;
    let pending_scroll = pending_source_anchor
        .as_ref()
        .and_then(|anchor| scroll_layout.source_anchor_top(anchor))
        .map(PendingReaderScroll::near_top)
        .or_else(|| focus_scroll_target(&focus_units, &focus_state, viewport))
        .or_else(|| {
            scroll_layout
                .frame_top(position)
                .map(PendingReaderScroll::near_top)
        });
    let status_prefix = if resume_failed {
        "保存位置已失效，已从开头打开"
    } else if resumed {
        "已恢复上次位置"
    } else {
        "已打开"
    };
    Ok(ReaderSurface {
        book_id: publication_id.to_owned(),
        title: title.to_owned().into(),
        document_sources,
        session,
        presenter: GpuiFramePresenter::new(),
        viewport,
        status: format!(
            "{status_prefix} · 第 {}/{} 页 · {} 条高亮",
            location.page_index + 1,
            location.page_count,
            highlights.len()
        )
        .into(),
        selection: None,
        selection_anchor: None,
        selection_focus: None,
        drag_anchor: None,
        dragging: false,
        progress_dirty: true,
        progress_store,
        highlight_store,
        highlights,
        scroll: ScrollHandle::new(),
        scroll_layout,
        focus_units,
        focus_state,
        pending_scroll,
        visible_frame_index,
        toc_items,
        sidebar: ReaderSidebar::None,
        annotation_editor: None,
        focus_chat: None,
        reading_mode: resolved_preferences.presentation.mode,
        selection_granularity: resolved_preferences.selection_granularity,
    })
}

fn focus_chat_annotation_action_summary(
    action: &AssistantAnnotationAction<StoredHighlight>,
) -> SharedString {
    let (verb, text) = match action {
        AssistantAnnotationAction::Create(annotation) => ("新增", annotation.quote.as_str()),
        AssistantAnnotationAction::Update(annotation) => {
            ("修改", annotation.note.as_deref().unwrap_or("清空批注"))
        }
        AssistantAnnotationAction::Delete { annotation_id } => ("删除", annotation_id.as_str()),
    };
    let mut clipped = text.chars().take(48).collect::<String>();
    if text.chars().count() > 48 {
        clipped.push('…');
    }
    format!("{verb}：{clipped}").into()
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("secondary-r", RefreshShelf, None),
            KeyBinding::new("escape", BackToShelf, None),
            KeyBinding::new("left", PreviousPage, None),
            KeyBinding::new("right", NextPage, None),
            KeyBinding::new("up", PreviousFocusUnit, None),
            KeyBinding::new("down", NextFocusUnit, None),
            KeyBinding::new("tab", ToggleFocusActions, None),
            KeyBinding::new("1", OpenFocusChat, None),
            KeyBinding::new("2", SaveFocusHighlight, None),
            KeyBinding::new("3", OpenFocusAnnotation, None),
            KeyBinding::new("alt", ToggleFocusFootnotes, None),
            KeyBinding::new("shift-left", ExtendSelectionLeft, None),
            KeyBinding::new("shift-right", ExtendSelectionRight, None),
            KeyBinding::new("secondary-a", SelectCurrentPage, None),
            KeyBinding::new("secondary-c", CopySelection, None),
            KeyBinding::new("secondary-h", SaveHighlight, None),
            KeyBinding::new("secondary-shift-o", ToggleContents, None),
            KeyBinding::new("secondary-shift-m", ToggleAnnotations, None),
            KeyBinding::new("secondary-shift-n", OpenAnnotation, None),
            KeyBinding::new("secondary-,", ToggleReaderSettings, None),
        ]);
        cx.bind_keys(text_input::key_bindings());
        cx.bind_keys([
            KeyBinding::new("enter", SaveAnnotation, Some("NoteInput")),
            KeyBinding::new("escape", CancelAnnotation, Some("NoteInput")),
            KeyBinding::new("enter", SendFocusChat, Some("ChatInput")),
            KeyBinding::new("escape", CancelFocusChat, Some("ChatInput")),
        ]);
        let bounds = Bounds::centered(None, size(px(980.0), px(720.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    window.focus(&focus_handle);
                    Shelf::load(focus_handle)
                })
            },
        )
        .expect("GPUI desktop shelf window should open");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;

    use super::*;
    use rebook_layout::{
        ReaderTypesetting, SpreadMode, text::legacy_parley::LegacyParleyTextEngine,
    };
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    const FIXTURE_ENTRIES: [&str; 5] = [
        "META-INF/container.xml",
        "OPS/package.opf",
        "OPS/nav.xhtml",
        "OPS/Styles/book.css",
        "OPS/Text/chapter.xhtml",
    ];

    #[test]
    fn cover_format_mapping_accepts_common_library_images() {
        let png = b"\x89PNG\r\n\x1a\n";
        assert!(cover_image(png).is_some());
        assert!(cover_image(b"not an image").is_none());
    }

    #[test]
    fn gpui_reader_starts_from_the_shared_unified_focus_contract() {
        let resolved = ReaderDocumentPreferences::default().resolve(true, false);
        assert_eq!(resolved.style.spread, SpreadMode::Scroll);
        assert_eq!(resolved.style.typesetting, ReaderTypesetting::unified());
        assert!(resolved.style.focus_footnote_icons);
        assert!(resolved.style.minimum_paragraph_gap.abs() < f32::EPSILON);
    }

    #[test]
    fn preference_relayout_preserves_source_anchor_and_selection_ranges() {
        let path = std::env::temp_dir().join(format!(
            "torto-gpui-preferences-{}-{}.epub",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        build_fixture(&path);
        let sync_path = path.with_extension("sqlite3");
        let progress_store =
            rebook_sync::SyncStore::open_at(sync_path.clone(), "preference-test").unwrap();
        let preferences = ReaderDocumentPreferences::default();
        let mut reader = open_reader_session(
            &path,
            "preference-fixture",
            "Preference Fixture",
            LayoutViewport::new(380, 220).unwrap(),
            LegacyParleyTextEngine::default,
            progress_store,
            &preferences,
        )
        .unwrap();
        reader.install_active_focus_selection().unwrap();
        let source_anchor = reader.focus_state.anchor().cloned().unwrap();
        let selected_ranges = reader.selection.as_ref().unwrap().ranges.clone();

        let mut changed = preferences;
        changed.typography.font_size = 26.0;
        changed.reading_mode = ReadingMode::Classic;
        changed.spread = SpreadMode::Double;
        changed.selection_granularity = SelectionGranularity::Sentence;
        reader.apply_document_preferences(&changed).unwrap();

        assert!((reader.session.style().typography.font_size - 26.0).abs() < f32::EPSILON);
        assert_eq!(reader.session.style().spread, SpreadMode::Double);
        assert_eq!(reader.reading_mode, ReadingMode::Classic);
        assert_eq!(reader.selection_granularity, SelectionGranularity::Sentence);
        assert_eq!(reader.focus_state.anchor(), Some(&source_anchor));
        assert_eq!(reader.selection.as_ref().unwrap().ranges, selected_ranges);
        let anchor_top = reader
            .scroll_layout
            .source_anchor_top(&source_anchor)
            .expect("source anchor must resolve after preference relayout");
        assert_eq!(
            reader.pending_scroll.map(|target| target.content_y),
            Some(anchor_top)
        );

        drop(reader);
        fs::remove_file(path).unwrap();
        let _ = fs::remove_file(&sync_path);
        let _ = fs::remove_file(sync_path.with_extension("sqlite3-wal"));
        let _ = fs::remove_file(sync_path.with_extension("sqlite3-shm"));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end fixture keeps open, interaction, persistence, and recovery assertions together"
    )]
    fn real_epub_opens_into_a_renderer_independent_reader_frame() {
        let path = std::env::temp_dir().join(format!(
            "torto-gpui-reader-{}-{}.epub",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        build_fixture(&path);
        let sync_path = path.with_extension("sqlite3");
        let progress_store =
            rebook_sync::SyncStore::open_at(sync_path.clone(), "test-device").unwrap();
        let mut reader_preferences = ReaderDocumentPreferences::default();
        reader_preferences.typography.font_size = 18.0;
        reader_preferences.selection_granularity = SelectionGranularity::Sentence;

        let mut reader = open_reader_session(
            &path,
            "fixture",
            "Fixture",
            LayoutViewport::new(360, 180).unwrap(),
            LegacyParleyTextEngine::default,
            progress_store.clone(),
            &reader_preferences,
        )
        .unwrap();
        assert!((reader.session.style().typography.font_size - 18.0).abs() < f32::EPSILON);
        assert_eq!(reader.reading_mode, ReadingMode::Focus);
        assert_eq!(reader.selection_granularity, SelectionGranularity::Sentence);
        let session_source = reader.session.book_source();
        assert!(Arc::ptr_eq(
            reader.document_sources.presented_source(),
            &session_source
        ));
        assert_eq!(
            reader
                .document_sources
                .presented_source()
                .parse_section(0)
                .unwrap(),
            reader
                .document_sources
                .canonical_source()
                .parse_section(0)
                .unwrap()
        );
        assert!(!reader.session.current_layout_frame().items.is_empty());
        assert!(!reader.scroll_layout.frames().is_empty());
        assert!(reader.scroll_layout.content_height() > 0.0);
        assert!(!reader.focus_units.is_empty());
        assert!(
            reader
                .focus_state
                .anchor()
                .is_some_and(|anchor| reader.focus_units[0].contains_anchor(anchor))
        );
        let hidden_chat_selection = reader.active_focus_selection().unwrap().0;
        assert!(!hidden_chat_selection.text.trim().is_empty());
        assert!(!hidden_chat_selection.ranges.is_empty());
        let annotation = StoredHighlight::with_note(
            reader.book_id.clone(),
            hidden_chat_selection.ranges.clone(),
            hidden_chat_selection.text.clone(),
            None,
        );
        let mut pending =
            PendingAnnotationActions::from_actions(vec![AssistantAnnotationAction::Create(
                annotation.clone(),
            )]);
        reader
            .confirm_pending_annotation_actions(&mut pending)
            .unwrap();
        assert!(pending.is_empty());
        assert_eq!(reader.highlights[0].ranges, hidden_chat_selection.ranges);
        let mut updated_annotation = annotation.clone();
        updated_annotation.note = Some("GPUI transaction".into());
        reader
            .apply_annotation_actions(&[AssistantAnnotationAction::Update(updated_annotation)])
            .unwrap();
        assert_eq!(
            reader.highlights[0].note.as_deref(),
            Some("GPUI transaction")
        );
        reader
            .apply_annotation_actions(&[AssistantAnnotationAction::Delete {
                annotation_id: annotation.id,
            }])
            .unwrap();
        assert!(reader.highlights.is_empty());
        let chat_context = focus_chat_reading_context(&reader);
        assert_eq!(chat_context.section_index, 0);
        assert_eq!(chat_context.unit_kind, "section");
        reader.focus_state.show_actions();
        reader.install_active_focus_selection().unwrap();
        assert!(
            reader
                .selection
                .as_ref()
                .is_some_and(|selection| !selection.text.trim().is_empty())
        );
        reader.focus_state.hide_actions();
        reader.clear_selection();
        assert_eq!(reader.session.location().section_index, 0);
        let toc_item = reader
            .toc_items
            .first()
            .expect("fixture publishes one navigation item")
            .clone();
        reader
            .session
            .go_to_href(toc_item.target.as_ref().expect("fixture TOC target"))
            .unwrap();
        assert_eq!(
            reader.session.snapshot().active_toc_id.as_deref(),
            Some(toc_item.id.as_str())
        );
        let result = reader.session.turn_page(PageDirection::Next).unwrap();
        assert_eq!(result.outcome, NavigationOutcome::Moved);
        reader.persist_progress().unwrap();
        let saved_locator = progress_store
            .load_progress("fixture")
            .unwrap()
            .expect("focus progress should be persisted")
            .locator;

        drop(reader);
        let restored = open_reader_session(
            &path,
            "fixture",
            "Fixture",
            LayoutViewport::new(440, 240).unwrap(),
            LegacyParleyTextEngine::default,
            progress_store.clone(),
            &reader_preferences,
        )
        .unwrap();
        assert!(restored.status.starts_with("已恢复上次位置"));
        let saved_anchor = saved_locator.source.unwrap().start;
        assert!(
            restored
                .session
                .current_layout_frame()
                .interaction()
                .contains_source_anchor(&saved_anchor)
        );
        let restored_anchor_top = restored.scroll_layout.source_anchor_top(&saved_anchor);
        assert!(restored_anchor_top.is_some());
        assert_eq!(
            restored.pending_scroll.map(|target| target.content_y),
            restored_anchor_top
        );
        assert_eq!(
            restored.pending_scroll.map(|target| target.viewport_y),
            Some(PendingReaderScroll::TOP_INSET)
        );

        drop(restored);
        let mut invalid_locator = progress_store
            .load_progress("fixture")
            .unwrap()
            .unwrap()
            .locator;
        invalid_locator.href = invalid_locator.href.resolve("missing.xhtml").unwrap();
        invalid_locator.progression = None;
        invalid_locator.total_progression = None;
        invalid_locator.source = None;
        progress_store
            .save_progress("fixture", &invalid_locator)
            .unwrap();
        let fallback = open_reader_session(
            &path,
            "fixture",
            "Fixture",
            LayoutViewport::new(440, 240).unwrap(),
            LegacyParleyTextEngine::default,
            progress_store,
            &reader_preferences,
        )
        .unwrap();
        assert!(fallback.status.starts_with("保存位置已失效，已从开头打开"));
        assert_eq!(fallback.session.location().page_index, 0);

        drop(fallback);
        fs::remove_file(path).unwrap();
        let _ = fs::remove_file(&sync_path);
        let _ = fs::remove_file(sync_path.with_extension("sqlite3-wal"));
        let _ = fs::remove_file(sync_path.with_extension("sqlite3-shm"));
    }

    fn build_fixture(output: &Path) {
        let fixture_root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-data/minimal-epub");
        let mut archive = ZipWriter::new(fs::File::create(output).unwrap());
        archive
            .start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(b"application/epub+zip").unwrap();
        for entry in FIXTURE_ENTRIES {
            archive
                .start_file(
                    entry,
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
                )
                .unwrap();
            archive
                .write_all(&fs::read(fixture_root.join(entry)).unwrap())
                .unwrap();
        }
        archive.finish().unwrap();
    }
}
