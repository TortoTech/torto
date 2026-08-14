use crate::plugins::TranslationMode;
use crate::settings::{AppliedSettings, ReaderSettingsChange};
use rebook_formats::BookFormat;
use rebook_publication::BookSource;
use rebook_publication::RenditionLayout;
use std::sync::Arc;

use super::{
    DesktopReader, FollowUp, NEXT_SEMANTIC_TASK_GENERATION, SearchMode, SemanticIndexTask,
    SnapshotEffects,
};
use std::sync::atomic::Ordering;

impl DesktopReader {
    pub(in crate::reader) fn request_settings(&mut self) {
        self.cancel_text_selection();
        self.close_overlay();
        self.settings_requested = true;
    }

    pub(crate) fn take_settings_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_requested)
    }

    pub(in crate::reader) fn request_settings_change(&mut self, change: ReaderSettingsChange) {
        if change == ReaderSettingsChange::ReadingMode(crate::preferences::ReadingMode::Focus)
            && !self.focus_mode_allowed()
        {
            self.notice_timer.show(
                &mut self.notice,
                self.language
                    .text(
                        "原始 PDF 不支持专注模式，请先切换到 OCR 版式",
                        "Focus mode requires the OCR reflow view for PDF",
                    )
                    .into(),
                std::time::Instant::now(),
            );
            return;
        }
        self.cancel_text_selection();
        self.close_overlay();
        self.settings_change_requested = Some(change);
    }

    pub(crate) fn take_settings_change_request(&mut self) -> Option<ReaderSettingsChange> {
        self.settings_change_requested.take()
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
        self.focus_units.clear();
        self.focus_target_offset = None;
        self.ui.focus_scroll_motion = None;
        self.ui.sidebar_pinned = true;
        self.set_sidebar_open(true);
        self.close_assistant_panel();
        self.ui.focus_actions_visible = false;
        self.focus_toc_override = None;
        self.cancel_text_selection();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "settings application coordinates reader, translation, and semantic follow-ups"
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
            || self.plugin_settings.providers != plugin_settings.providers
            || (self.language != language
                && plugin_settings.target_language == crate::plugins::TARGET_LANGUAGE_INTERFACE);
        let toc_translation_setting_changed =
            self.plugin_settings.translate_toc != plugin_settings.translate_toc;
        let old_embedding_provider = self
            .plugin_settings
            .providers
            .iter()
            .find(|provider| provider.id == self.plugin_settings.embedding_provider);
        let new_embedding_provider = plugin_settings
            .providers
            .iter()
            .find(|provider| provider.id == plugin_settings.embedding_provider);
        let semantic_backend_changed = self.plugin_settings.embedding_provider
            != plugin_settings.embedding_provider
            || self.plugin_settings.embedding_model != plugin_settings.embedding_model
            || old_embedding_provider != new_embedding_provider;
        let semantic_enable_changed =
            self.plugin_settings.semantic_search_enabled != plugin_settings.semantic_search_enabled;
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
        let pdf_ocr_reflow_changed =
            self.plugin_settings.pdf_ocr_reflow_enabled != plugin_settings.pdf_ocr_reflow_enabled;

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
        self.reading_mode = reading_mode;
        let mut style = self.reader.style();
        style.spread = if self.reading_mode == crate::preferences::ReadingMode::Focus {
            rebook_layout::SpreadMode::Scroll
        } else {
            settings.spread
        };
        style.minimum_paragraph_gap = if self.reading_mode == crate::preferences::ReadingMode::Focus
        {
            super::FOCUS_MINIMUM_PARAGRAPH_GAP
        } else {
            0.0
        };
        style.typography.clone_from(&settings.typography);
        self.selection_granularity = settings.selection_granularity;
        super::apply_theme_colors(&mut style, settings.theme);
        match self.reader.set_style(style) {
            Ok(snapshot) => {
                self.plugin_settings = plugin_settings;
                self.language = language;
                if mode_changed {
                    if !self.is_focus_mode() {
                        self.restore_book_chat_session();
                    }
                    self.focus_units.clear();
                    self.focus_target_offset = None;
                    self.ui.focus_scroll_motion = None;
                    self.ui.sidebar_pinned =
                        self.reading_mode == crate::preferences::ReadingMode::Classic;
                    self.set_sidebar_open(
                        self.reading_mode == crate::preferences::ReadingMode::Classic,
                    );
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
                } else if toc_translation_setting_changed && !self.plugin_settings.translate_toc {
                    self.translation.toc_task.cancel();
                }
                if !self.plugin_settings.semantic_search_enabled {
                    self.semantic_index.task.cancel();
                    self.semantic_index.progress.clear();
                    self.search.mode = SearchMode::Text;
                } else if semantic_enable_changed || semantic_backend_changed {
                    self.semantic_index.task.cancel();
                    let semantic_source: Arc<dyn BookSource> = self.rewrite_source.clone();
                    self.semantic_index.task.begin(SemanticIndexTask {
                        source: semantic_source,
                        settings: self.plugin_settings.clone(),
                        generation: NEXT_SEMANTIC_TASK_GENERATION.fetch_add(1, Ordering::Relaxed),
                    });
                    self.semantic_index.progress = language.text("索引中 0%", "Indexing 0%").into();
                    if semantic_enable_changed {
                        self.search.mode = SearchMode::Semantic;
                    }
                }
                if !self.plugin_settings.ocr_enabled {
                    self.pdf_toc.task.cancel();
                    self.pdf_toc.progress.clear();
                } else if toc_recognition_enable_changed || ocr_backend_changed {
                    self.start_pdf_metadata_extraction();
                }
                self.apply_snapshot(
                    snapshot,
                    SnapshotEffects {
                        translation: FollowUp::Run,
                        ..SnapshotEffects::static_content_change()
                    },
                );
                self.queue_toc_translation();
                if pdf_ocr_reflow_changed && self.pdf_ocr.available {
                    self.persist_progress();
                    self.reopen_notice = Some(
                        language
                            .text("已更新 PDF OCR 内容重排", "PDF OCR content reflow updated")
                            .into(),
                    );
                    self.reopen_requested = Some(self.source_path.clone());
                }
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
