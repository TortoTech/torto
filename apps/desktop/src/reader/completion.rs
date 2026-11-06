use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::{DesktopReader, ReaderFramePlan};
use crate::{preferences::AppLanguage, statistics};

type SummaryResult = Result<CompletionSummary, String>;

#[derive(Default)]
struct CompletionSummary {
    elapsed_ms: u64,
    finished: bool,
    finished_books: usize,
    started: Option<u64>,
    last: Option<u64>,
    days: usize,
    annotations: usize,
    notes: usize,
    history: Vec<(String, u64)>,
}

pub(super) struct CompletionPage {
    summary: Option<CompletionSummary>,
    pending: Option<Receiver<SummaryResult>>,
    error: Option<String>,
    mark_requested: bool,
    scroll_offset: f32,
}

impl CompletionPage {
    fn new() -> Self {
        Self {
            summary: None,
            pending: None,
            error: None,
            mark_requested: false,
            scroll_offset: 0.0,
        }
    }

    fn poll(&mut self) {
        let Some(pending) = &self.pending else { return };
        let result = match pending.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Reading statistics worker stopped".into()),
        };
        self.pending = None;
        match result {
            Ok(summary) => {
                self.summary = Some(summary);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

fn load_summary(book_id: &str, store: Option<&crate::sync::SyncStore>) -> SummaryResult {
    statistics::flush_checked()?;
    let books = statistics::aggregate(
        &statistics::events(&statistics::database().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
    );
    let finished_books = books
        .values()
        .filter(|b| b.status == statistics::Status::Finished)
        .count();
    let annotations = store
        .map(|s| s.annotations_for_book(book_id))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let notes = annotations
        .iter()
        .filter(|a| a.note.as_ref().is_some_and(|n| !n.trim().is_empty()))
        .count();
    let mut summary = CompletionSummary {
        finished_books,
        annotations: annotations.len(),
        notes,
        ..Default::default()
    };
    if let Some(book) = books.get(book_id) {
        summary.elapsed_ms = statistics::union_duration(&book.intervals);
        summary.finished = book.status == statistics::Status::Finished;
        summary.started = book.started;
        summary.last = book.last;
        summary.days = book.days.len();
        summary.history = book
            .days
            .iter()
            .rev()
            .filter(|(_, ms)| **ms > 0)
            .take(2)
            .map(|(day, ms)| (day.clone(), *ms))
            .collect();
    }
    Ok(summary)
}

impl DesktopReader {
    pub(super) fn open_completion_page(&mut self) {
        if self.completion.is_some() {
            return;
        }
        self.statistics
            .tick(false, false, self.snapshot.total_progression);
        self.statistics.flush_pending();
        self.cancel_text_selection();
        self.ui.focus_actions_visible = false;
        self.ui.focus_footnotes_visible = false;
        self.ui.focus_scroll_motion = None;
        self.ui.focus_navigation_repeat = Default::default();
        self.pending_page_turn = None;
        self.pending_reading_unit_turn = None;
        self.pending_focus_wheel_turn = None;
        self.selected_image = None;
        self.completion = Some(CompletionPage::new());
    }

    fn load_completion_summary(&mut self, ctx: &egui::Context) {
        let Some(page) = self.completion.as_mut() else {
            return;
        };
        if page.pending.is_some() {
            return;
        }
        let (send, receive) = mpsc::channel();
        page.pending = Some(receive);
        page.error = None;
        let book_id = self.book_id.clone();
        let store = self.progress_store.clone();
        let ctx = ctx.clone();
        // One job per page. Dropping the page drops the receiver, so results
        // from an earlier book/end-page cannot replace the current screen.
        std::thread::spawn(move || {
            let result = load_summary(&book_id, store.as_ref());
            if send.send(result).is_ok() {
                ctx.request_repaint();
            }
        });
    }

    fn close_completion_page(&mut self, ctx: &egui::Context) {
        self.completion = None;
        self.pending_page_turn = None;
        self.pending_reading_unit_turn = None;
        self.pending_focus_wheel_turn = None;
        self.ui.focus_navigation_repeat = Default::default();
        self.scroll_target_position = None;
        // This is a synthetic page: do not issue a native previous turn.
        ctx.memory_mut(|m| {
            m.stop_text_input();
            m.move_focus(egui::FocusDirection::None);
        });
        self.bump_scene_revision();
        ctx.request_repaint();
    }

    pub(super) fn completion_page_ui(
        &mut self,
        root: &mut egui::Ui,
        blocked: bool,
    ) -> ReaderFramePlan {
        let ctx = root.ctx().clone();
        self.completion.as_mut().unwrap().poll();
        if self
            .completion
            .as_ref()
            .is_some_and(|p| p.summary.is_none() && p.pending.is_none() && p.error.is_none())
        {
            self.load_completion_summary(&ctx);
        }
        let (backwards, exit_shortcut) = ctx.input_mut(|i| {
            if blocked {
                return (false, false);
            }
            let exit = i.consume_shortcut(&self.shortcuts.return_to_shelf);
            // Use the configured shortcuts, including custom chords. Repeats
            // from the forward turn that opened this page must not dismiss it.
            let mut back = false;
            for shortcut in [
                self.shortcuts.previous_page_or_paragraph,
                self.shortcuts.previous_section,
                egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::PageUp),
                egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Escape),
            ] {
                let fresh = super::egui_view::shortcut_has_fresh_press(i, &shortcut);
                back |= i.consume_shortcut(&shortcut) && fresh;
            }
            (back, exit)
        });
        if backwards {
            // egui schedules spatial focus traversal before widgets run.
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        let colors = crate::ui::palette();
        let language = self.language;
        let scroll_was_at_top = self.completion.as_ref().unwrap().scroll_offset <= 0.5;
        let mut scroll_offset = 0.0;
        let mut rect = egui::Rect::NOTHING;
        let (mut mark, mut exit, mut retry) = (false, false, false);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(colors.background)).show(root, |ui| {
            rect = ui.available_rect_before_wrap();
            let width = (rect.width() - 32.0).clamp(1.0, 600.0);
            // Reserve the bottom action while allowing the statistics to scroll
            // in small windows. No database access or polling timer on the UI.
            let body = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), (rect.bottom()-76.0).max(rect.top())));
            ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                let scroll = egui::ScrollArea::vertical().id_salt("completion-body").auto_shrink([false, false]).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(width);
                        ui.add_space(24.0);
                        let page = self.completion.as_ref().unwrap();
                        let summary = page.summary.as_ref();
                        let finished = summary.is_some_and(|s| s.finished);
                        ui.label(egui::RichText::new(if finished {language.text("已读完", "Finished")} else {language.text("全书完", "The End")}).size(32.0).strong().color(colors.text));
                        ui.add_space(20.0);
                        let (badge, _) = ui.allocate_exact_size(egui::vec2(156.0, 156.0), egui::Sense::hover());
                        ui.painter().circle_filled(badge.center(), 78.0, if finished {colors.accent} else {colors.surface});
                        if finished {
                            ui.painter().text(badge.center()+egui::vec2(0.0,-38.0), egui::Align2::CENTER_CENTER, language.text("累计读完", "Books finished"), egui::FontId::proportional(14.0), egui::Color32::WHITE);
                            let count = summary.unwrap().finished_books;
                            let label = if language.resolved()==AppLanguage::SimplifiedChinese {format!("{count}本")} else {count.to_string()};
                            ui.painter().text(badge.center()+egui::vec2(0.0,13.0), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(46.0), egui::Color32::WHITE);
                        } else {
                            ui.painter().text(badge.center(), egui::Align2::CENTER_CENTER, language.text("完", "END"), egui::FontId::proportional(if language.resolved()==AppLanguage::SimplifiedChinese {92.0} else {42.0}), colors.accent);
                            ui.add_space(10.0);
                            mark = ui.add_enabled(!blocked && page.pending.is_none() && summary.is_some() && !page.mark_requested,
                                egui::Button::new(egui::RichText::new(language.text("标记读完", "Mark finished")).size(16.0).color(colors.accent)).frame(false)).clicked();
                        }
                        ui.add_space(24.0);
                        egui::Frame::new().fill(colors.surface).corner_radius(16).inner_margin(20.0).show(ui, |ui| {
                            ui.set_width((width-40.0).max(1.0));
                            let column = ((ui.available_width()-24.0)/2.0).max(1.0);
                            ui.horizontal_top(|ui| {
                                metric(ui, column, language.text("累计时长", "Total reading time"), |ui| {
                                    if let Some(s)=summary {ui.label(crate::statistics::detail_reading_time_text(s.elapsed_ms,language,26.0));} else {value(ui,None);}
                                    caption(ui, &summary.and_then(|s| s.started).map(|ms| format!("{} {}", statistics::date(Some(ms)), language.text("开始阅读", "started"))).unwrap_or_default());
                                });
                                ui.add_space(16.0);
                                metric(ui, column, language.text("阅读天数", "Reading days"), |ui| {
                                    value(ui, summary.map(|s| format!("{} {}", s.days, language.text("天", "days"))));
                                    caption(ui, &summary.and_then(|s| s.last).map(|ms| format!("{} {}", language.text("上次阅读：", "Last read:"), statistics::date(Some(ms)))).unwrap_or_default());
                                });
                            });
                            ui.add_space(24.0);
                            ui.horizontal_top(|ui| {
                                metric(ui, column, language.text("想法笔记", "Notes and highlights"), |ui| {
                                    value(ui, summary.map(|s| s.annotations.to_string()));
                                    caption(ui, &summary.map(|s| if language.resolved()==AppLanguage::SimplifiedChinese {format!("{} 处高亮 · {} 条批注", s.annotations-s.notes,s.notes)} else {format!("{} highlights · {} comments",s.annotations-s.notes,s.notes)}).unwrap_or_default());
                                });
                                ui.add_space(16.0);
                                metric(ui, column, language.text("阅读明细", "Reading history"), |ui| {
                                    if let Some(s)=summary {
                                        let maximum=s.history.iter().map(|(_,ms)| *ms).max().unwrap_or(1).max(1) as f32;
                                        if s.history.is_empty() {caption(ui,language.text("暂无阅读明细","No reading history yet"));}
                                        for (day,ms) in &s.history {
                                            caption(ui,day);
                                            let (bar,_) = ui.allocate_exact_size(egui::vec2(column,5.0),egui::Sense::hover());
                                            ui.painter().rect_filled(bar,3,colors.accent_soft);
                                            ui.painter().rect_filled(egui::Rect::from_min_size(bar.min,egui::vec2(bar.width()*(*ms as f32/maximum),bar.height())),3,colors.accent);
                                        }
                                    } else {value(ui,None);}
                                });
                            });
                            if let Some(error)=&page.error {
                                ui.add_space(12.0);
                                ui.colored_label(colors.error_text,error);
                                retry=ui.add_enabled(!blocked && page.pending.is_none(),egui::Button::new(language.text("重新加载统计","Reload statistics"))).clicked();
                            }
                        });
                        ui.add_space(24.0);
                    });
                });
                scroll_offset = scroll.state.clamped_offset().y;
            });
            let footer=egui::Rect::from_min_max(egui::pos2(rect.left(), (rect.bottom()-68.0).max(rect.top())),rect.max);
            ui.scope_builder(egui::UiBuilder::new().max_rect(footer),|ui| {
                ui.vertical_centered(|ui| {
                    ui.set_max_width(width);
                    ui.add_space(8.0);
                    exit=ui.add_enabled(!blocked,egui::Button::new(egui::RichText::new(language.text("退出阅读","Exit reading")).size(16.0).color(egui::Color32::WHITE)).fill(colors.accent).corner_radius(12).min_size(egui::vec2(width,44.0))).clicked();
                });
            });
        });
        self.completion.as_mut().unwrap().scroll_offset = scroll_offset;
        if mark {
            self.completion.as_mut().unwrap().mark_requested = true;
            self.statistics.mark_finished();
            self.load_completion_summary(&ctx);
        } else if retry {
            self.load_completion_summary(&ctx);
        }
        let wheel_back = !blocked
            && scroll_was_at_top
            && ctx.input(|i| {
                i.pointer.hover_pos().is_some_and(|p| rect.contains(p))
                    && i.events
                        .iter()
                        .any(|e| matches!(e,egui::Event::MouseWheel {delta,..} if delta.y>0.0))
            });
        if exit || exit_shortcut {
            self.request_exit();
        } else if backwards || wheel_back {
            self.close_completion_page(&ctx);
        }
        ctx.set_cursor_icon(egui::CursorIcon::Default);
        ReaderFramePlan {
            rect,
            scene_id: self.scene_id,
            scene_revision: self.scene_revision,
            content_left: 0.0,
            preview_left: 0.0,
            reflow_pending: false,
            live_resize_preview: false,
            background: peniko::Color::from_rgba8(
                colors.background.r(),
                colors.background.g(),
                colors.background.b(),
                255,
            ),
        }
    }
}

fn metric(ui: &mut egui::Ui, width: f32, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            caption(ui, title);
            ui.add_space(4.0);
            content(ui);
        },
    );
}
fn value(ui: &mut egui::Ui, text: Option<String>) {
    ui.label(
        egui::RichText::new(text.unwrap_or_else(|| "—".into()))
            .size(26.0)
            .color(crate::ui::palette().text),
    );
}
fn caption(ui: &mut egui::Ui, text: &str) {
    if !text.is_empty() {
        ui.label(
            egui::RichText::new(text)
                .size(12.0)
                .color(crate::ui::palette().muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 850.0),
            )),
            events,
            ..Default::default()
        }
    }
    fn key(shortcut: egui::KeyboardShortcut, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key: shortcut.logical_key,
            physical_key: None,
            pressed,
            repeat,
            modifiers: shortcut.modifiers,
        }
    }
    fn run(
        ctx: &egui::Context,
        input: egui::RawInput,
        ui: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(input, ui);
        output.textures_delta.clear();
        output
    }
    fn page(reader: &mut DesktopReader) {
        // Avoid touching a real statistics database in navigation/UI tests.
        reader.completion = Some(CompletionPage {
            summary: Some(CompletionSummary::default()),
            pending: None,
            error: None,
            mark_requested: false,
            scroll_offset: 0.0,
        });
    }

    #[test]
    fn completion_previous_shortcuts_restore_the_exact_page_with_focused_buttons() {
        for focus in [false, true] {
            for custom in [false, true] {
                for section in [false, true] {
                    let (mut reader, _, _) = super::super::semantic_layout::tests::fixture();
                    reader.reading_mode = if focus {
                        super::super::ReadingMode::Focus
                    } else {
                        super::super::ReadingMode::Classic
                    };
                    if custom {
                        reader.shortcuts.previous_page_or_paragraph =
                            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::J);
                        reader.shortcuts.previous_section =
                            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::K);
                    }
                    let shortcut = if section {
                        reader.shortcuts.previous_section
                    } else {
                        reader.shortcuts.previous_page_or_paragraph
                    };
                    let before = reader.reader.current_locator();
                    let focus_before = reader.focus_unit_index;
                    page(&mut reader);
                    let ctx = egui::Context::default();
                    // Draw first, then focus an actual completion button as after
                    // a mouse click or Tab; arrow navigation must still win.
                    let _ = run(&ctx, input(vec![]), |ui| {
                        reader.ui(ui, None, false);
                    });
                    let tab = egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Tab);
                    let _ = run(&ctx, input(vec![key(tab, true, false)]), |ui| {
                        reader.ui(ui, None, false);
                    });
                    assert!(
                        ctx.memory(|m| m.focused().is_some()),
                        "a completion action must have keyboard focus"
                    );
                    let _ = run(&ctx, input(vec![key(shortcut, true, false)]), |ui| {
                        reader.ui(ui, None, false);
                    });
                    assert!(
                        reader.completion.is_none(),
                        "focus={focus} custom={custom} section={section}"
                    );
                    assert_eq!(reader.reader.current_locator(), before);
                    assert_eq!(reader.focus_unit_index, focus_before);
                    assert!(reader.pending_page_turn.is_none());
                    assert!(reader.pending_reading_unit_turn.is_none());
                    let _ = run(&ctx, input(vec![key(shortcut, false, false)]), |ui| {
                        reader.ui(ui, None, false);
                    });
                    assert_eq!(reader.reader.current_locator(), before);
                }
            }
        }
    }

    #[test]
    fn completion_respects_blocked_input_and_stays_idle_after_loading() {
        let (mut reader, _, _) = super::super::semantic_layout::tests::fixture();
        page(&mut reader);
        let ctx = egui::Context::default();
        let shortcut = reader.shortcuts.previous_section;
        let _ = run(&ctx, input(vec![key(shortcut, true, false)]), |ui| {
            reader.ui(ui, None, true);
        });
        assert!(reader.completion.is_some());
        let _ = run(&ctx, input(vec![key(shortcut, false, false)]), |ui| {
            reader.ui(ui, None, false);
        });
        let _ = run(
            &ctx,
            input(vec![key(reader.shortcuts.next_section, true, false)]),
            |ui| {
                reader.ui(ui, None, false);
            },
        );
        assert!(reader.completion.is_some());
        for _ in 0..5 {
            let _ = run(&ctx, input(vec![]), |ui| {
                reader.ui(ui, None, false);
            });
        }
        let output = run(&ctx, input(vec![]), |ui| {
            reader.ui(ui, None, false);
        });
        let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        assert!(
            delay > std::time::Duration::from_secs(60),
            "completion must not schedule idle timer frames: {delay:?}"
        );
    }
}
