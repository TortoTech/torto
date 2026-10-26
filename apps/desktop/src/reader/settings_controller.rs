use crate::plugins::TranslationMode;
use crate::settings::{AppliedSettings, ReaderSettingsChange};
use rebook_formats::BookFormat;
use rebook_publication::RenditionLayout;

use super::{DesktopReader, FollowUp, SnapshotEffects};

#[cfg(test)]
#[path = "settings_controller_tests.rs"]
mod tests;

impl DesktopReader {
    pub(crate) fn prepare_feedback_request(&mut self) {
        self.cancel_text_selection();
        self.close_overlay();
    }

    pub(crate) fn take_feedback_request(&mut self) -> bool {
        std::mem::take(&mut self.feedback_requested)
    }

    pub(crate) fn feedback_context(&self, ctx: &egui::Context) -> crate::feedback::OpenContext {
        use crate::feedback::{Book, Location, Reader, Translation, book_document, clipped};
        let mut seed = crate::feedback::OpenContext::shelf(ctx);
        seed.context.source = "reader".into();
        let original = self
            .pdf_ocr_controller
            .as_ref()
            .map(|controller| controller.original_source());
        let book = original.as_deref().unwrap_or(self.source.as_ref()).book();
        let title = clipped(&book.metadata.title, 500);
        seed.context.book = Some(Book {
            title: if title.is_empty() {
                "(missing original title)".into()
            } else {
                title
            },
            authors: book
                .metadata
                .authors
                .iter()
                .map(|v| clipped(v, 200))
                .filter(|v| !v.is_empty())
                .take(30)
                .collect(),
            languages: book
                .metadata
                .languages
                .iter()
                .map(|v| clipped(v, 40))
                .filter(|v| !v.is_empty())
                .take(12)
                .collect(),
            format: self.format.label().into(),
            layout: if book.metadata.layout == RenditionLayout::PrePaginated {
                "fixed"
            } else {
                "reflowable"
            }
            .into(),
        });
        let active = self
            .snapshot
            .active_toc_id
            .as_ref()
            .and_then(|id| self.reader.toc_items().iter().find(|item| &item.id == id));
        let href = self
            .source
            .book()
            .sections
            .get(self.snapshot.location.section_index)
            .map(|s| &s.href);
        let focus_unit = self.focus_units.get(self.focus_unit_index).filter(|unit| {
            self.is_focus_mode()
                && self.focus_units_ready
                && self.source
                    .book()
                    .sections
                    .get(unit.position.section_index)
                    .is_some_and(|section| section.id == unit.range.start.spine)
        });
        let focus_document = focus_unit
            .and_then(|unit| self.source.book().sections.get(unit.position.section_index))
            .and_then(|section| book_document(section.href.path()));
        let focus = focus_unit
            .filter(|_| focus_document.is_some())
            .and_then(|unit| {
                crate::feedback::FocusLocation::from_source(&unit.range, &unit.paint_ranges)
            });
        seed.context.location = Some(Location {
            chapter: active
                .map(|item| clipped(&item.label, 500))
                .filter(|v| !v.is_empty()),
            document: focus_document.or_else(|| href.and_then(|href| book_document(href.path()))),
            anchor: active
                .and_then(|item| item.target.as_ref())
                .and_then(|href| href.fragment())
                .map(|v| clipped(v, 500))
                .filter(|v| !v.is_empty()),
            page: (self.format == BookFormat::Pdf
                && self.source.book().metadata.layout == RenditionLayout::PrePaginated)
                .then_some(self.snapshot.location.section_index + 1),
            focus,
        });
        let pdf = self.format == BookFormat::Pdf;
        let reflow = pdf && self.pdf_ocr.mode == super::PdfOcrViewMode::Reflow;
        if reflow {
            seed.pdf_id = Some(self.book_id.clone());
        }
        seed.context.reader = Some(Reader {
            mode: if self.is_focus_mode() {
                "focus"
            } else {
                "classic"
            }
            .into(),
            typesetting: if self.reader.style().typesetting.mode
                == rebook_layout::TypesettingMode::Unified
            {
                "unified"
            } else {
                "original"
            }
            .into(),
            pdf_reflow: (pdf && !reflow).then(|| "off".into()),
            translation: Translation {
                enabled: self.translation.render_enabled,
                target_language: clipped(
                    &self.plugin_settings.resolved_target_language(
                        crate::preferences::AppLanguage::system_translation_target(),
                    ),
                    40,
                ),
            },
        });
        seed
    }

    pub(in crate::reader) fn request_settings(&mut self) {
        self.cancel_text_selection();
        self.close_overlay();
        self.settings_requested = true;
    }

    pub(crate) fn take_settings_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_requested)
    }

    pub(in crate::reader) fn pdf_focus_mode_hint(&self) -> &'static str {
        if self.pdf_ocr.available {
            self.language.text(
                "专注模式需要文字版式，请先切换。",
                "Focus mode requires text reflow. Switch to the text view first.",
            )
        } else {
            self.language.text(
                "请先生成 PDF 文字，再使用专注模式。",
                "Generate PDF text before using Focus mode.",
            )
        }
    }

    pub(in crate::reader) fn request_settings_change(&mut self, change: ReaderSettingsChange) {
        if change == ReaderSettingsChange::ReadingMode(crate::preferences::ReadingMode::Focus)
            && !self.focus_mode_allowed()
        {
            let hint = self.pdf_focus_mode_hint();
            self.notice_timer
                .show(&mut self.notice, hint.into(), std::time::Instant::now());
            return;
        }
        self.cancel_text_selection();
        self.close_overlay();
        self.settings_change_requested = Some(change);
    }

    pub(crate) fn take_settings_change_request(&mut self) -> Option<ReaderSettingsChange> {
        self.settings_change_requested.take()
    }

    pub(in crate::reader) fn toggle_focus_cursor_visibility(&mut self) {
        if !self.is_focus_mode() {
            return;
        }
        let hidden = super::resolved_focus_cursor_hidden(
            self.hide_cursor_in_focus_mode,
            self.focus_cursor_hidden_override,
        );
        self.focus_cursor_hidden_override = Some(!hidden);
    }

    pub(crate) fn report_settings_error(&mut self, error: String) {
        self.error = Some(error);
    }

    pub(super) fn leave_focus_mode_for_pdf(&mut self) {
        if !self.is_focus_mode() {
            return;
        }
        self.reading_mode = crate::preferences::ReadingMode::Classic;
        self.restore_book_chat_session();
        self.invalidate_focus_units();
        self.focus_target_offset = None;
        self.ui.focus_scroll_motion = None;
        let sidebar = self.ui.classic_sidebar;
        self.ui.sidebar_pinned = sidebar.pinned;
        self.ui.sidebar_width = sidebar.width;
        self.set_sidebar_open(sidebar.open);
        self.close_assistant_panel();
        self.ui.focus_actions_visible = false;
        self.focus_toc_override = None;
        self.cancel_text_selection();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "settings application coordinates reader and translation follow-ups"
    )]
    pub(crate) fn apply_global_settings(&mut self, settings: &AppliedSettings) {
        let mut plugin_settings = settings.plugin_settings.clone();
        if self.format == BookFormat::Pdf
            && self.source.book().metadata.layout == RenditionLayout::PrePaginated
        {
            plugin_settings.translation_mode = TranslationMode::Replace;
        }
        let language = settings.language;
        let translation_backend_changed = self.plugin_settings.translation_provider
            != plugin_settings.translation_provider
            || self.plugin_settings.translation_model != plugin_settings.translation_model
            || self.plugin_settings.target_language != plugin_settings.target_language
            || self.plugin_settings.providers != plugin_settings.providers;
        let toc_translation_setting_changed =
            self.plugin_settings.translate_toc != plugin_settings.translate_toc;
        let old_ocr_provider = self
            .plugin_settings
            .providers
            .iter()
            .find(|provider| provider.id == self.plugin_settings.ocr_provider);
        let new_ocr_provider = plugin_settings
            .providers
            .iter()
            .find(|provider| provider.id == plugin_settings.ocr_provider);
        let ocr_backend_changed = self.plugin_settings.ocr_provider != plugin_settings.ocr_provider
            || self.plugin_settings.ocr_model != plugin_settings.ocr_model
            || old_ocr_provider != new_ocr_provider;
        let toc_recognition_enable_changed =
            self.plugin_settings.ocr_enabled != plugin_settings.ocr_enabled;
        let translation_mode_changed =
            self.plugin_settings.translation_mode != plugin_settings.translation_mode;

        if let Err(error) = self
            .translation_source
            .set_mode(plugin_settings.translation_mode)
            .and_then(|()| {
                if translation_backend_changed {
                    self.translation_source.clear()
                } else {
                    Ok(())
                }
            })
        {
            self.error = Some(format!(
                "{}: {error}",
                language.text("应用翻译设置失败", "Failed to apply translation settings")
            ));
            return;
        }

        let reading_mode = if settings.reading_mode == crate::preferences::ReadingMode::Focus
            && !self.focus_mode_allowed()
        {
            crate::preferences::ReadingMode::Classic
        } else {
            settings.reading_mode
        };
        let mode_changed = self.reading_mode != reading_mode;
        if mode_changed {
            self.ui.classic_sidebar = self.classic_sidebar_state();
        }
        self.reading_mode = reading_mode;
        let mut style = self.reader.style();
        style.spread = if self.reading_mode == crate::preferences::ReadingMode::Focus {
            rebook_layout::SpreadMode::Scroll
        } else {
            settings.spread
        };
        style.focus_footnote_icons =
            self.source.book().metadata.layout != RenditionLayout::PrePaginated;
        style.minimum_paragraph_gap = 0.0;
        style.minimum_content_width = if self.is_focus_mode() { 400.0 } else { 0.0 };
        style.minimum_horizontal_margin = if self.is_focus_mode() { 24.0 } else { 0.0 };
        style.typography.clone_from(&settings.typography);
        style.typesetting = super::effective_typesetting(self.reading_mode, &settings.typesetting);
        self.selection_granularity = settings.selection_granularity;
        super::apply_theme_colors(&mut style, crate::ui::theme());
        let citation_mode_changed = self.semantic_source.set_unified_citations(
            style.focus_footnote_icons
                && style.typesetting.mode == rebook_layout::TypesettingMode::Unified,
        );
        let previous_style = self.reader.style();
        let updated = if citation_mode_changed {
            self.reader.refresh_source_with_style(style)
        } else {
            self.reader.set_style(style)
        };
        match updated {
            Ok(snapshot) => {
                let content_changed = mode_changed
                    || self.reader.style() != previous_style
                    || citation_mode_changed
                    || translation_backend_changed
                    || translation_mode_changed;
                self.plugin_settings = plugin_settings;
                self.language = language;
                if self.focus_style != settings.focus_style {
                    self.focus_style = settings.focus_style;
                    self.bump_scene_revision();
                }
                if self.hide_cursor_in_focus_mode != settings.hide_cursor_in_focus_mode {
                    self.focus_cursor_hidden_override = None;
                }
                self.hide_cursor_in_focus_mode = settings.hide_cursor_in_focus_mode;
                self.shortcuts.clone_from(&settings.shortcuts);
                if mode_changed {
                    self.ui.focus_footnotes_visible = false;
                    self.ui.focus_footnote_scroll_delta = 0.0;
                    if !self.is_focus_mode() {
                        self.restore_book_chat_session();
                    }
                    self.invalidate_focus_units();
                    self.focus_target_offset = None;
                    self.ui.focus_scroll_motion = None;
                    let sidebar = self.ui.classic_sidebar.for_mode(self.reading_mode);
                    self.ui.sidebar_pinned = sidebar.pinned;
                    self.ui.sidebar_width = sidebar.width;
                    self.set_sidebar_open(sidebar.open);
                    self.close_assistant_panel();
                    self.ui.focus_actions_visible = false;
                    self.focus_toc_override = None;
                    self.cancel_text_selection();
                }
                self.sync_settings.clone_from(&settings.sync_settings);
                self.sync_password.clone_from(&settings.sync_password);
                self.translation.clear_error();
                if translation_backend_changed {
                    self.translation.task.cancel();
                    self.translation.toc_task.cancel();
                    self.translation.toc_labels.clear();
                    self.translation.canonical_titles.clear();
                } else if toc_translation_setting_changed && !self.plugin_settings.translate_toc {
                    self.translation.toc_task.cancel();
                }
                if !self.plugin_settings.ocr_enabled {
                    self.pdf_toc.task.cancel();
                    self.pdf_toc.progress.clear();
                } else if toc_recognition_enable_changed || ocr_backend_changed {
                    self.start_pdf_metadata_extraction();
                }
                if content_changed {
                    self.apply_snapshot(
                        snapshot,
                        SnapshotEffects {
                            translation: FollowUp::Run,
                            ..SnapshotEffects::static_content_change()
                        },
                    );
                } else {
                    // Opening reapplies the global settings snapshot. Retain the
                    // worker-prepared scroll layout and precise restored anchor
                    // when the body source and style are already identical.
                    self.queue_visible_section_translation();
                }
                self.queue_toc_translation();
            }
            Err(error) => {
                self.error = Some(format!(
                    "{}: {error}",
                    language.text("应用阅读设置失败", "Failed to apply reading settings")
                ));
            }
        }
    }
}
