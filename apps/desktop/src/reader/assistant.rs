use std::sync::Arc;
use std::time::Instant;

use rebook_publication::{Block, BookSource, Inline, RenditionLayout, SourceRange};
use rebook_reader::ReaderVisibleTextFragment;

use crate::platform::UserEvent;
use crate::plugins::{
    BookSearchResult, ChatAnnotationAction, ChatCommand, ChatCommandResolution, ChatReadingContext,
    ChatRequestKind, ChatResponse, ChatRole, ChatSelection, ChatTurn, PDF_PAGE_ANCHOR_PREFIX,
    PdfOcrViewMode, TranslationBlockInput, chat_citation_link, chat_with_book,
    extract_pdf_metadata, recognize_pdf, resolve_chat_command, search_book, section_title,
    set_pdf_ocr_view_mode, translate_blocks, translate_blocks_incremental,
};
use crate::semantic;

use super::chat_autocomplete::{
    ChatReference, ChatReferenceKind, build_chat_prompt_with_references,
    chat_reference_suggestions, chat_reference_token, insert_chat_reference, parse_chat_citation,
};
use super::{
    AssistantPanel, ChatStreamMessage, ChatStreamingState, ChatTask, ChatTaskMessage,
    DesktopReader, FocusedMark, MarkRetention, PdfMetadataUpdate, PdfOcrTask, PdfOcrTaskMessage,
    PdfTocTask, PdfTocTaskMessage, SearchMode, SearchTask, SearchTaskMessage,
    SemanticIndexTaskMessage, SidebarTab, SnapshotEffects, TocTranslationTask,
    TocTranslationTaskMessage, TranslationTask, TranslationTaskMessage,
};

impl DesktopReader {
    #[allow(
        clippy::too_many_lines,
        reason = "reader background work is dispatched from one event-loop integration point"
    )]
    pub(crate) fn spawn_pending_tasks(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if let Some(request) = self.semantic_index.task.take_pending() {
            let proxy = proxy.clone();
            runtime.spawn(async move {
                let id = request.id;
                let payload = request.payload;
                let generation = payload.generation;
                let progress_proxy = proxy.clone();
                let result = semantic::index_book(
                    payload.source,
                    payload.settings,
                    move |completed, total| {
                        let _ = progress_proxy.send_event(UserEvent::ReaderSemanticIndex(
                            SemanticIndexTaskMessage::Progress {
                                id,
                                generation,
                                completed,
                                total,
                            },
                        ));
                    },
                )
                .await;
                let _ = proxy.send_event(UserEvent::ReaderSemanticIndex(
                    SemanticIndexTaskMessage::Complete {
                        generation,
                        message: crate::async_task::TaskResult { id, result },
                    },
                ));
            });
        }
        if let Some(request) = self.search.task.take_pending() {
            let proxy = proxy.clone();
            runtime.spawn(async move {
                let id = request.id;
                let payload = request.payload;
                let result = match payload.mode {
                    SearchMode::Text => tokio::task::spawn_blocking(move || {
                        search_book(payload.source.as_ref(), &payload.query, 200)
                    })
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result),
                    SearchMode::Semantic => semantic::search(
                        &payload.query,
                        &payload.book_id,
                        payload.scope,
                        &payload.settings,
                        Some(50),
                        false,
                    )
                    .await
                    .map(|results| {
                        results
                            .into_iter()
                            .map(|result| BookSearchResult {
                                book_id: result.book_id,
                                book_title: result.book_title,
                                section_index: result.section_index,
                                section_title: result.section_title,
                                excerpt: semantic_result_excerpt(&result.text),
                                matched_text: result.text,
                                block_kind: result.block_kind,
                                range: result.range,
                                similarity: Some(result.similarity),
                                is_image: result.modality
                                    == crate::semantic::SemanticModality::Image,
                                image_href: result.image_href,
                                image_preview: result.image_preview,
                            })
                            .collect()
                    }),
                };
                let _ = proxy.send_event(UserEvent::ReaderSearch(SearchTaskMessage { id, result }));
            });
        }
        if let Some(request) = self.chat.task.take_pending() {
            let proxy = proxy.clone();
            crate::diagnostics::log(
                "chat.task.start",
                &[
                    crate::diagnostics::Field::U64("id", request.id),
                    crate::diagnostics::Field::Usize(
                        "history_turns",
                        request.payload.history.len(),
                    ),
                    crate::diagnostics::Field::Usize(
                        "question_chars",
                        request.payload.question.chars().count(),
                    ),
                ],
            );
            let stream_proxy = proxy.clone();
            runtime.spawn(async move {
                let id = request.id;
                let payload = request.payload;
                let result = chat_with_book(
                    payload.source,
                    payload.format,
                    payload.kind,
                    payload.rewrite_source,
                    payload.book_id,
                    payload.selection,
                    payload.annotations,
                    payload.settings,
                    payload.history,
                    payload.question,
                    payload.current,
                    payload.response_language,
                    move |content| {
                        let _ = stream_proxy.send_event(UserEvent::ReaderChatStream(
                            ChatStreamMessage { id, content },
                        ));
                    },
                )
                .await;
                let _ = proxy.send_event(UserEvent::ReaderChat(ChatTaskMessage { id, result }));
            });
        }
        if let Some(request) = self.translation.task.take_pending() {
            let proxy = proxy.clone();
            runtime.spawn(async move {
                let id = request.id;
                let payload = request.payload;
                let batch_proxy = proxy.clone();
                let result = translate_blocks_incremental(
                    payload.settings,
                    payload.blocks,
                    move |translations| {
                        let _ = batch_proxy.send_event(UserEvent::ReaderTranslation(
                            TranslationTaskMessage::Batch { id, translations },
                        ));
                    },
                )
                .await;
                let _ = proxy.send_event(UserEvent::ReaderTranslation(
                    TranslationTaskMessage::Complete(crate::async_task::TaskResult { id, result }),
                ));
            });
        }
        if let Some(request) = self.translation.toc_task.take_pending() {
            let proxy = proxy.clone();
            runtime.spawn(async move {
                let id = request.id;
                let payload = request.payload;
                let result = translate_blocks(payload.settings, payload.blocks).await;
                let _ =
                    proxy.send_event(UserEvent::ReaderTocTranslation(TocTranslationTaskMessage {
                        id,
                        result,
                    }));
            });
        }
        self.spawn_pending_pdf_toc(runtime, proxy);
        self.spawn_pending_pdf_ocr(runtime, proxy);
    }

    fn spawn_pending_pdf_ocr(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if let Some(request) = self.pdf_ocr.task.take_pending() {
            let proxy = proxy.clone();
            let progress_proxy = proxy.clone();
            runtime.spawn(async move {
                let id = request.id;
                let payload = request.payload;
                let result = recognize_pdf(
                    payload.path,
                    payload.book_id,
                    payload.page_count,
                    payload.settings,
                    move |message| {
                        let _ = progress_proxy.send_event(UserEvent::ReaderPdfOcr(
                            PdfOcrTaskMessage::Progress { id, message },
                        ));
                    },
                )
                .await;
                let _ = proxy.send_event(UserEvent::ReaderPdfOcr(PdfOcrTaskMessage::Complete(
                    crate::async_task::TaskResult { id, result },
                )));
            });
        }
    }

    fn spawn_pending_pdf_toc(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if let Some(request) = self.pdf_toc.task.take_pending() {
            let proxy = proxy.clone();
            let progress_proxy = proxy.clone();
            runtime.spawn(async move {
                let id = request.id;
                let payload = request.payload;
                let result = extract_pdf_metadata(
                    payload.source,
                    payload.settings,
                    payload.need_toc,
                    payload.missing.any(),
                    move |message| {
                        let _ = progress_proxy.send_event(UserEvent::ReaderPdfToc(
                            PdfTocTaskMessage::Progress { id, message },
                        ));
                    },
                )
                .await;
                let _ = proxy.send_event(UserEvent::ReaderPdfToc(PdfTocTaskMessage::Complete(
                    crate::async_task::TaskResult { id, result },
                )));
            });
        }
    }

    pub(super) fn start_pdf_toc_generation(&mut self) {
        if self.pdf_toc.task.is_pending()
            || self.format != rebook_formats::BookFormat::Pdf
            || !self.plugin_settings.ocr_enabled
        {
            return;
        }
        self.pdf_toc.error = None;
        self.pdf_toc.draft = None;
        self.pdf_toc.editing = false;
        self.pdf_toc.progress = "正在准备页面…".into();
        self.pdf_toc.task.begin(PdfTocTask {
            source: self.pdf_ocr_controller.as_ref().map_or_else(
                || Arc::clone(&self.source),
                |controller| controller.original_source(),
            ),
            book_id: self.book_id.clone(),
            need_toc: true,
            missing: self.pdf_metadata_missing,
            settings: self.plugin_settings.clone(),
        });
    }

    pub(super) fn start_pdf_metadata_extraction(&mut self) {
        let need_toc = self.source.book().table_of_contents.is_empty();
        if self.pdf_toc.task.is_pending()
            || self.format != rebook_formats::BookFormat::Pdf
            || !self.plugin_settings.ocr_enabled
            || (!need_toc && !self.pdf_metadata_missing.any())
        {
            return;
        }
        self.pdf_toc.error = None;
        self.pdf_toc.progress = self
            .language
            .text("正在准备元数据提取…", "Preparing metadata extraction…")
            .into();
        self.pdf_toc.task.begin(PdfTocTask {
            source: self.pdf_ocr_controller.as_ref().map_or_else(
                || Arc::clone(&self.source),
                |controller| controller.original_source(),
            ),
            book_id: self.book_id.clone(),
            need_toc,
            missing: self.pdf_metadata_missing,
            settings: self.plugin_settings.clone(),
        });
    }

    pub(super) fn start_pdf_ocr(&mut self) {
        if self.pdf_ocr.task.is_pending()
            || self.format != rebook_formats::BookFormat::Pdf
            || !self.plugin_settings.pdf_ocr_enabled
        {
            return;
        }
        self.pdf_ocr.progress = self.language.text("正在准备 PDF…", "Preparing PDF…").into();
        self.pdf_ocr.task.begin(PdfOcrTask {
            path: self.source_path.clone(),
            book_id: self.book_id.clone(),
            page_count: self.source.book().sections.len(),
            settings: self.plugin_settings.clone(),
        });
    }

    pub(crate) fn complete_pdf_ocr(&mut self, message: PdfOcrTaskMessage) {
        match message {
            PdfOcrTaskMessage::Progress { id, message } => {
                if self.pdf_ocr.task.in_flight(id).is_some() {
                    self.pdf_ocr.progress = message;
                }
            }
            PdfOcrTaskMessage::Complete(message) => {
                let Some(_request) = self.pdf_ocr.task.complete(message.id) else {
                    return;
                };
                self.pdf_ocr.progress.clear();
                match message.result {
                    Ok(()) => {
                        self.pdf_ocr.available = true;
                        self.pdf_ocr.mode = PdfOcrViewMode::Reflow;
                        self.persist_progress();
                        self.reopen_notice = Some(
                            self.language
                                .text(
                                    "PDF OCR 解析成功，已切换到 OCR 版式",
                                    "PDF OCR completed; switched to OCR reflow",
                                )
                                .into(),
                        );
                        self.reopen_requested = Some(self.source_path.clone());
                    }
                    Err(error) => {
                        let prefix = self.language.text("PDF OCR 解析失败", "PDF OCR failed");
                        self.error_timer.show(
                            &mut self.error,
                            format!("{prefix}：{error}"),
                            Instant::now(),
                        );
                    }
                }
            }
        }
    }

    pub(super) fn toggle_pdf_ocr_view(&mut self) -> bool {
        if !self.pdf_ocr.available || self.pdf_ocr.task.is_pending() {
            return false;
        }
        let Some(controller) = self.pdf_ocr_controller.clone() else {
            return false;
        };
        let previous_mode = self.pdf_ocr.mode;
        let mode = match self.pdf_ocr.mode {
            PdfOcrViewMode::Original => PdfOcrViewMode::Reflow,
            PdfOcrViewMode::Reflow => PdfOcrViewMode::Original,
        };
        let navigation_target = match self.pdf_ocr.mode {
            PdfOcrViewMode::Original => {
                controller.reflow_target_for_page(self.reader.location().section_index)
            }
            PdfOcrViewMode::Reflow => self
                .reader
                .current_preceding_anchor(PDF_PAGE_ANCHOR_PREFIX)
                .and_then(|fragment| controller.original_target_for_reflow_anchor(&fragment)),
        };
        match set_pdf_ocr_view_mode(&self.book_id, mode) {
            Ok(()) => {
                self.persist_progress();
                controller.set_mode(mode);
                let fixed_page = mode == PdfOcrViewMode::Original;
                self.translation_source
                    .set_fixed_page_replacement_only(fixed_page);
                let _ = self
                    .translation_source
                    .set_mode(self.plugin_settings.translation_mode);
                let mut style = self.reader.style().clone();
                style.column_gap = if fixed_page {
                    0.0
                } else {
                    rebook_layout::ReaderStyle::default().column_gap
                };
                match self
                    .reader
                    .refresh_source_with_style_at_href(style, navigation_target.as_ref())
                {
                    Ok(snapshot) => {
                        self.pdf_ocr.mode = mode;
                        self.apply_snapshot(snapshot, SnapshotEffects::static_content_change());
                        true
                    }
                    Err(error) => {
                        controller.set_mode(previous_mode);
                        let previous_fixed_page = previous_mode == PdfOcrViewMode::Original;
                        self.translation_source
                            .set_fixed_page_replacement_only(previous_fixed_page);
                        let _ = self
                            .translation_source
                            .set_mode(self.plugin_settings.translation_mode);
                        let _ = set_pdf_ocr_view_mode(&self.book_id, previous_mode);
                        self.error_timer.show(
                            &mut self.error,
                            format!(
                                "{}: {error}",
                                self.language
                                    .text("切换 PDF 版式失败", "Failed to switch PDF layout")
                            ),
                            Instant::now(),
                        );
                        false
                    }
                }
            }
            Err(error) => {
                self.error_timer.show(
                    &mut self.error,
                    format!(
                        "{}: {error}",
                        self.language
                            .text("切换 PDF 版式失败", "Failed to switch PDF layout")
                    ),
                    Instant::now(),
                );
                false
            }
        }
    }

    pub(crate) fn complete_pdf_toc(
        &mut self,
        message: PdfTocTaskMessage,
    ) -> Option<PdfMetadataUpdate> {
        match message {
            PdfTocTaskMessage::Progress { id, message } => {
                if self.pdf_toc.task.in_flight(id).is_some() {
                    self.pdf_toc.progress = message;
                }
                None
            }
            PdfTocTaskMessage::Complete(message) => {
                let request = self.pdf_toc.task.complete(message.id)?;
                self.pdf_toc.progress.clear();
                match message.result {
                    Ok(mut extraction) => {
                        let update = extraction.metadata.take().and_then(|metadata| {
                            self.apply_recognized_pdf_metadata(
                                &request.book_id,
                                request.missing,
                                &metadata,
                            )
                        });
                        if request.missing.any() && update.is_none() {
                            self.error_timer.show(
                                &mut self.error,
                                self.language
                                    .text(
                                        "没有识别出 PDF 缺失的标题或作者",
                                        "The missing PDF title or authors could not be recognized",
                                    )
                                    .into(),
                                Instant::now(),
                            );
                        }
                        if let Some(draft) = extraction.toc {
                            self.pdf_toc.error = None;
                            self.pdf_toc.draft = Some(draft);
                            self.pdf_toc.editing = false;
                            self.apply_generated_toc();
                        } else if request.need_toc {
                            self.pdf_toc.error = extraction.toc_error;
                        }
                        update
                    }
                    Err(error) => {
                        if request.need_toc {
                            self.pdf_toc.error = Some(error);
                        } else {
                            self.error_timer.show(
                                &mut self.error,
                                format!(
                                    "{}: {error}",
                                    self.language.text(
                                        "PDF 元数据提取失败",
                                        "PDF metadata extraction failed"
                                    )
                                ),
                                Instant::now(),
                            );
                        }
                        None
                    }
                }
            }
        }
    }

    fn apply_recognized_pdf_metadata(
        &mut self,
        book_id: &str,
        missing: super::PdfMetadataMissing,
        metadata: &crate::generated_metadata::GeneratedPdfMetadata,
    ) -> Option<PdfMetadataUpdate> {
        let title_recognized = missing.title && !metadata.title.is_empty();
        let authors_recognized = missing.authors && !metadata.authors.is_empty();
        if !title_recognized && !authors_recognized {
            return None;
        }
        if title_recognized {
            self.display_metadata.title.clone_from(&metadata.title);
            self.pdf_metadata_missing.title = false;
        }
        if authors_recognized {
            self.display_metadata.authors.clone_from(&metadata.authors);
            self.pdf_metadata_missing.authors = false;
        }
        let mut cached = crate::generated_metadata::load(book_id)
            .unwrap_or_default()
            .unwrap_or_default();
        if !metadata.title.is_empty() {
            cached.title.clone_from(&metadata.title);
        }
        if !metadata.authors.is_empty() {
            cached.authors.clone_from(&metadata.authors);
        }
        cached.provider_name.clone_from(&metadata.provider_name);
        cached.model.clone_from(&metadata.model);
        if let Err(error) = crate::generated_metadata::save(book_id, &cached) {
            self.error_timer.show(
                &mut self.error,
                format!(
                    "{}: {error}",
                    self.language
                        .text("保存 PDF 元数据缓存失败", "Failed to cache PDF metadata")
                ),
                Instant::now(),
            );
        } else {
            self.show_notice(
                self.language
                    .text("PDF 元数据提取完成", "PDF metadata extracted")
                    .into(),
            );
        }
        Some(PdfMetadataUpdate {
            book_id: book_id.to_owned(),
            title: self.display_metadata.title.clone(),
            authors: self.display_metadata.authors.clone(),
        })
    }

    pub(super) fn open_search(&mut self) {
        self.ui.sidebar_tab = SidebarTab::Search;
        self.search.focus_input = true;
        self.set_sidebar_open(true);
    }

    pub(super) fn start_search(&mut self) {
        if self.search.task.is_pending() {
            return;
        }
        let query = self.search.query.trim().to_owned();
        if query.is_empty() {
            self.search.status = self
                .language
                .text("请输入搜索内容", "Enter a search query")
                .into();
            return;
        }
        if self.search.mode == SearchMode::Semantic
            && self.search.scope == crate::semantic::SemanticSearchScope::CurrentBook
            && self.semantic_index.task.is_pending()
        {
            self.search.status = self
                .language
                .text(
                    "当前书仍在建立语义索引，请稍后再试",
                    "The current book is still being indexed. Try again shortly.",
                )
                .into();
            return;
        }
        self.search.status = self.language.text("正在搜索…", "Searching…").into();
        self.search.results.clear();
        self.focused_mark = None;
        self.search.task.begin(SearchTask {
            source: Arc::clone(&self.source),
            query,
            mode: self.search.mode,
            scope: self.search.scope,
            book_id: self.book_id.clone(),
            settings: self.plugin_settings.clone(),
        });
        self.bump_scene_revision();
    }

    pub(crate) fn complete_search(&mut self, message: SearchTaskMessage) {
        let Some(request) = self.search.task.complete(message.id) else {
            return;
        };
        match message.result {
            Ok(results) => {
                self.search.status = if results.is_empty() {
                    if request.mode == SearchMode::Semantic {
                        self.language
                            .text("没有找到相关内容", "No related passages found")
                            .into()
                    } else {
                        self.language
                            .text("没有找到匹配内容", "No matches found")
                            .into()
                    }
                } else {
                    match self.language {
                        crate::preferences::AppLanguage::SimplifiedChinese => {
                            if request.mode == SearchMode::Semantic {
                                format!("找到 {} 条相关内容", results.len())
                            } else {
                                format!("找到 {} 处结果", results.len())
                            }
                        }
                        crate::preferences::AppLanguage::English => {
                            if request.mode == SearchMode::Semantic {
                                format!("Found {} related passages", results.len())
                            } else {
                                format!("Found {} matches", results.len())
                            }
                        }
                    }
                };
                self.search.results = results;
            }
            Err(error) => {
                self.search.results.clear();
                if request.mode == SearchMode::Semantic {
                    self.search.status.clear();
                    self.error_timer.show(
                        &mut self.error,
                        format!(
                            "{}: {error}",
                            self.language.text("语义搜索失败", "Semantic search failed")
                        ),
                        Instant::now(),
                    );
                } else {
                    self.search.status = error;
                }
            }
        }
    }

    pub(crate) fn complete_semantic_index(&mut self, message: SemanticIndexTaskMessage) {
        match message {
            SemanticIndexTaskMessage::Progress {
                id,
                generation,
                completed,
                total,
            } => {
                if self
                    .semantic_index
                    .task
                    .in_flight(id)
                    .is_some_and(|task| task.generation == generation)
                {
                    let percentage = semantic_index_percentage(completed, total);
                    self.semantic_index.progress = match self.language {
                        crate::preferences::AppLanguage::SimplifiedChinese => {
                            format!("索引中 {percentage}%")
                        }
                        crate::preferences::AppLanguage::English => {
                            format!("Indexing {percentage}%")
                        }
                    };
                }
            }
            SemanticIndexTaskMessage::Complete {
                generation,
                message,
            } => {
                if self
                    .semantic_index
                    .task
                    .in_flight(message.id)
                    .is_none_or(|task| task.generation != generation)
                {
                    return;
                }
                if self.semantic_index.task.complete(message.id).is_none() {
                    return;
                }
                match message.result {
                    Ok(summary) => {
                        tracing::debug!(
                            already_indexed = summary.already_indexed,
                            total_chunks = summary.total_chunks,
                            "semantic index ready"
                        );
                        self.semantic_index.progress.clear();
                    }
                    Err(error) => {
                        self.semantic_index.progress.clear();
                        self.error_timer.show(
                            &mut self.error,
                            format!(
                                "{}: {error}",
                                self.language
                                    .text("语义索引失败", "Semantic indexing failed")
                            ),
                            Instant::now(),
                        );
                    }
                }
            }
        }
    }

    pub(crate) fn take_search_navigation_request(&mut self) -> Option<BookSearchResult> {
        self.search_navigation_requested.take()
    }

    pub(crate) fn report_search_navigation_error(&mut self, message: String) {
        self.search.status = message;
    }

    pub(crate) fn go_to_search_result(&mut self, result: &BookSearchResult) {
        if result.book_id != self.book_id {
            self.search_navigation_requested = Some(result.clone());
            return;
        }
        let navigation = if result.is_image {
            self.reader.go_to_section(result.section_index)
        } else {
            self.reader.go_to_source(&result.range.start)
        };
        match navigation {
            Ok(navigation) => {
                self.focused_mark =
                    (!result.is_image).then(|| FocusedMark::search(result.range.clone()));
                self.apply_snapshot(navigation.snapshot, SnapshotEffects::navigation());
            }
            Err(error) => {
                self.search.status = format!(
                    "{}: {error}",
                    self.language
                        .text("搜索结果跳转失败", "Failed to open search result")
                );
            }
        }
    }

    pub(super) fn toggle_assistant_panel(&mut self, panel: AssistantPanel) {
        self.log_diagnostic_snapshot("assistant.toggle.before", None);
        self.cancel_text_selection();
        if self.ui.assistant_panel == Some(panel) && self.ui.assistant_motion.target > 0.5 {
            self.close_assistant_panel();
        } else {
            self.open_assistant_panel(panel);
        }
        self.log_diagnostic_snapshot("assistant.toggle.after", None);
    }

    fn open_assistant_panel(&mut self, panel: AssistantPanel) {
        self.ui.assistant_panel = Some(panel);
        self.chat.move_cursor_to_end = true;
        if self.ui.assistant_motion.animate_to(1.0) {
            self.ui.last_motion_tick = Some(std::time::Instant::now());
        }
    }

    pub(super) fn close_assistant_panel(&mut self) {
        self.log_diagnostic_snapshot("assistant.close.before", None);
        if self.ui.assistant_motion.animate_to(0.0) {
            self.ui.last_motion_tick = Some(std::time::Instant::now());
        }
        self.log_diagnostic_snapshot("assistant.close.after", None);
    }

    pub(super) fn send_chat(&mut self) {
        let raw = self.chat.input.trim().to_owned();
        if (raw.is_empty() && self.chat.references.is_empty()) || self.chat.task.is_pending() {
            return;
        }
        match resolve_chat_command(&raw) {
            ChatCommandResolution::MissingArguments {
                message,
                insert_text,
            } => {
                self.chat.messages.push(ChatTurn {
                    role: ChatRole::User,
                    content: raw.clone(),
                    display_content: Some(raw),
                });
                self.chat.messages.push(ChatTurn {
                    role: ChatRole::Assistant,
                    content: message,
                    display_content: None,
                });
                self.chat.input = insert_text.into();
                self.chat.cursor_char_index = self.chat.input.chars().count();
                self.chat.move_cursor_to_end = true;
                self.chat.suggestion_index = 0;
                self.chat.error = None;
            }
            ChatCommandResolution::Resolved {
                display,
                prompt,
                kind,
            } => {
                let references = std::mem::take(&mut self.chat.references);
                let prompt = build_chat_prompt_with_references(
                    &prompt,
                    &references,
                    self.language == crate::preferences::AppLanguage::English,
                );
                self.chat.input.clear();
                self.chat.cursor_char_index = 0;
                self.chat.suggestion_index = 0;
                self.queue_chat_with_kind(prompt, Some(display), kind);
            }
            ChatCommandResolution::NotCommand | ChatCommandResolution::Unknown => {
                let references = std::mem::take(&mut self.chat.references);
                let display = if raw.is_empty() {
                    references
                        .iter()
                        .map(|reference| format!("@{}", reference.label))
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    raw.clone()
                };
                let prompt = build_chat_prompt_with_references(
                    &raw,
                    &references,
                    self.language == crate::preferences::AppLanguage::English,
                );
                self.chat.input.clear();
                self.chat.cursor_char_index = 0;
                self.chat.suggestion_index = 0;
                self.queue_chat(prompt, (!references.is_empty()).then_some(display));
            }
        }
    }

    pub(super) fn select_chat_command(&mut self, command: ChatCommand) {
        if !self.chat.task.is_pending() {
            self.chat.input = command.insert_text.into();
            self.chat.cursor_char_index = self.chat.input.chars().count();
            self.chat.suggestion_index = 0;
            self.chat.move_cursor_to_end = true;
            self.chat.error = None;
        }
    }

    pub(super) fn current_chat_reference_suggestions(&mut self) -> Vec<ChatReference> {
        let Some(token) = chat_reference_token(
            &self.chat.input,
            self.chat.cursor_char_index,
            &self.chat.references,
        ) else {
            return Vec::new();
        };
        self.refresh_chat_reference_options();
        chat_reference_suggestions(
            &self.chat.reference_options,
            &self.chat.references,
            &token.query,
        )
    }

    pub(super) fn select_chat_reference(&mut self, reference: ChatReference) {
        if self.chat.task.is_pending() {
            return;
        }
        let Some(token) = chat_reference_token(
            &self.chat.input,
            self.chat.cursor_char_index,
            &self.chat.references,
        ) else {
            return;
        };
        let (input, cursor_char_index) =
            insert_chat_reference(&self.chat.input, &token, &reference);
        if !self
            .chat
            .references
            .iter()
            .any(|item| item.id == reference.id)
        {
            self.chat.references.push(reference);
        }
        self.chat.input = input;
        self.chat.cursor_char_index = cursor_char_index;
        self.chat.suggestion_index = 0;
        self.chat.move_cursor_to_end = true;
        self.chat.error = None;
    }

    pub(super) fn remove_chat_reference(&mut self, id: &str) {
        self.chat.references.retain(|reference| reference.id != id);
    }

    fn refresh_chat_reference_options(&mut self) {
        let location = (
            self.snapshot.location.section_index,
            self.snapshot.location.segment_index,
            self.snapshot.location.page_index,
        );
        if self.chat.reference_options_location == Some(location) {
            return;
        }
        let section_index = location.0;
        let english = self.language == crate::preferences::AppLanguage::English;
        let book_title = self.source.book().metadata.title.trim().to_owned();
        let mut options = vec![ChatReference {
            id: "book:full-text".into(),
            kind: ChatReferenceKind::Book,
            label: if english { "Full text" } else { "全文" }.into(),
            description: if book_title.is_empty() {
                if english { "Entire book" } else { "整本书" }.into()
            } else {
                book_title
            },
            link: "link://book".into(),
            excerpt: None,
        }];

        let mut section_titles = Vec::new();
        if let Ok(section) = self.source.parse_section(section_index) {
            let title = section_title(self.source.as_ref(), section_index, &section.blocks);
            section_titles.push((section_index, title.clone()));
            options.push(ChatReference {
                id: format!("section:{section_index}"),
                kind: ChatReferenceKind::Section,
                label: title.clone(),
                description: if english {
                    format!("Current chapter · {}", section_index + 1)
                } else {
                    format!("当前章节 · {}", section_index + 1)
                },
                link: chat_citation_link(section_index, None),
                excerpt: None,
            });
        }

        let Ok(fragments) = self.reader.current_visible_text_fragments() else {
            self.chat.reference_options = options;
            return;
        };
        let visible_paragraphs = visible_chat_paragraphs(fragments);
        for (paragraph_index, (section_index, node, part_index, text)) in
            visible_paragraphs.into_iter().enumerate()
        {
            let title_index = section_titles
                .iter()
                .position(|(candidate, _)| *candidate == section_index);
            let title_index = title_index.unwrap_or_else(|| {
                let title = self.source.parse_section(section_index).map_or_else(
                    |_| {
                        format!(
                            "{} {}",
                            if english { "Chapter" } else { "章节" },
                            section_index + 1
                        )
                    },
                    |section| section_title(self.source.as_ref(), section_index, &section.blocks),
                );
                section_titles.push((section_index, title));
                section_titles.len() - 1
            });
            options.push(paragraph_reference(
                section_index,
                paragraph_index + 1,
                &section_titles[title_index].1,
                &node,
                part_index,
                &text,
                english,
            ));
            if options.len() >= 120 {
                break;
            }
        }
        self.chat.reference_options = options;
        self.chat.reference_options_location = Some(location);
    }

    pub(super) fn explain_selection(&mut self) {
        let Some(selection) = self.selection.clone() else {
            return;
        };
        let selected_text = selection.text.trim();
        let english = self.language == crate::preferences::AppLanguage::English;
        let question = match self.language {
            crate::preferences::AppLanguage::SimplifiedChinese => format!(
                "请结合所引用的原文语境解释选中的内容。说明它的直接含义、在本段中的作用，以及理解它所需的背景；不要脱离原文进行无依据推测。\n\n选中文字：\n{selected_text}"
            ),
            crate::preferences::AppLanguage::English => format!(
                "Explain the selected text using the referenced source context. Cover its direct meaning, its role in the passage, and any background needed to understand it. Do not speculate beyond the source.\n\nSelected text:\n{selected_text}"
            ),
        };
        let references = selection_reference(
            self.source.as_ref(),
            &selection.ranges,
            selected_text,
            english,
        )
        .into_iter()
        .collect::<Vec<_>>();
        let prompt = build_chat_prompt_with_references(&question, &references, english);
        let display_content = Some(if english {
            format!("Explain: “{}”", clip_chat_reference_text(selected_text, 72))
        } else {
            format!("解释：“{}”", clip_chat_reference_text(selected_text, 72))
        });
        self.focused_mark = Some(FocusedMark::assistant(selection.ranges.clone()));
        self.cancel_text_selection();
        self.open_assistant_panel(AssistantPanel::Chat);
        self.queue_chat(prompt, display_content);
    }

    pub(super) fn open_chat_citation(&mut self, locator: &str) {
        let locators = locator
            .lines()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let citations = locators
            .iter()
            .filter_map(|locator| parse_chat_citation(locator))
            .collect::<Vec<_>>();
        let Some(citation) = citations.first() else {
            self.chat.error = Some(
                self.language
                    .text("引用链接无效", "Invalid citation link")
                    .into(),
            );
            return;
        };
        if citations.len() != locators.len()
            || citations
                .iter()
                .any(|candidate| candidate.section_index != citation.section_index)
        {
            self.chat.error = Some(
                self.language
                    .text("引用链接无效", "Invalid citation link")
                    .into(),
            );
            return;
        }
        let target_ranges = citations
            .iter()
            .filter_map(|citation| {
                citation.node.as_deref().and_then(|node| {
                    source_range_for_node(self.source.as_ref(), citation.section_index, node)
                })
            })
            .collect::<Vec<_>>();
        let target_range = target_ranges.first();
        let result = if let Some(range) = &target_range {
            self.reader.go_to_source(&range.start)
        } else {
            self.reader.go_to_section(citation.section_index)
        };
        match result {
            Ok(result) => {
                self.focused_mark =
                    (!target_ranges.is_empty()).then(|| FocusedMark::assistant(target_ranges));
                self.apply_snapshot(
                    result.snapshot,
                    SnapshotEffects {
                        marks: MarkRetention::ClearSelectedHighlight,
                        ..SnapshotEffects::navigation()
                    },
                );
            }
            Err(error) => {
                self.chat.error = Some(format!(
                    "{}: {error}",
                    self.language
                        .text("无法跳转到引用", "Unable to open citation")
                ));
            }
        }
    }

    pub(super) fn queue_chat(&mut self, question: String, display_content: Option<String>) {
        self.queue_chat_with_kind(question, display_content, ChatRequestKind::Normal);
    }

    fn queue_chat_with_kind(
        &mut self,
        question: String,
        display_content: Option<String>,
        kind: ChatRequestKind,
    ) {
        if let Err(error) = self.plugin_settings.chat_endpoint() {
            crate::diagnostics::log(
                "chat.queue.rejected",
                &[
                    crate::diagnostics::Field::Text("reason", "invalid_endpoint"),
                    crate::diagnostics::Field::Usize("error_chars", error.chars().count()),
                ],
            );
            self.chat.error = Some(error);
            self.open_assistant_panel(AssistantPanel::Chat);
            return;
        }
        let history = self.chat.messages.clone();
        self.chat.messages.push(ChatTurn {
            role: ChatRole::User,
            content: question.clone(),
            display_content,
        });
        self.chat.error = None;
        let history_turns = history.len();
        let question_chars = question.chars().count();
        let question_lines = question.lines().count();
        let current = self.chat_reading_context();
        let id = self.chat.task.begin(ChatTask {
            source: Arc::clone(&self.source),
            format: self.format,
            kind,
            rewrite_source: Arc::clone(&self.rewrite_source),
            book_id: self.book_id.clone(),
            selection: self.selection.as_ref().map(|selection| ChatSelection {
                text: selection.text.clone(),
                ranges: selection.ranges.clone(),
            }),
            annotations: self.highlights.clone(),
            settings: self.plugin_settings.clone(),
            history,
            question,
            current,
            response_language: self.language.translation_target().into(),
        });
        self.chat.streaming = Some(ChatStreamingState {
            task_id: id,
            content: String::new(),
        });
        crate::diagnostics::log(
            "chat.queue",
            &[
                crate::diagnostics::Field::U64("id", id),
                crate::diagnostics::Field::Usize("history_turns", history_turns),
                crate::diagnostics::Field::Usize("question_chars", question_chars),
                crate::diagnostics::Field::Usize("question_lines", question_lines),
            ],
        );
    }

    fn chat_reading_context(&self) -> ChatReadingContext {
        let location = self.snapshot.location;
        let active_toc = self
            .snapshot
            .active_toc_id
            .as_deref()
            .and_then(|id| self.reader.toc_items().iter().find(|item| item.id == id));
        let toc_label = active_toc.map(|item| item.label.clone());
        let toc_href = active_toc
            .and_then(|item| item.target.as_ref())
            .map(ToString::to_string);
        let book = self.source.book();
        let fixed_page = book.metadata.layout == RenditionLayout::PrePaginated;
        let spine = book.sections.get(location.section_index);
        let current_title = if fixed_page {
            toc_label
                .clone()
                .unwrap_or_else(|| format!("第 {} 页", location.section_index + 1))
        } else {
            section_title(self.source.as_ref(), location.section_index, &[])
        };
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
            section_title: if fixed_page {
                None
            } else {
                Some(current_title)
            },
            toc_label,
            toc_href,
            section_fraction,
            total_fraction: self.snapshot.total_progression,
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

    pub(crate) fn complete_chat(&mut self, message: ChatTaskMessage) {
        if self.chat.task.complete(message.id).is_none() {
            crate::diagnostics::log(
                "chat.complete.stale",
                &[crate::diagnostics::Field::U64("id", message.id)],
            );
            return;
        }
        self.chat.streaming = None;
        match message.result {
            Ok(response) => {
                log_completed_chat(message.id, &response);
                if !response.rewrite_transactions.is_empty() {
                    match self.reader.refresh_source() {
                        Ok(snapshot) => {
                            self.apply_snapshot(
                                snapshot,
                                SnapshotEffects {
                                    marks: MarkRetention::ClearAll,
                                    ..SnapshotEffects::static_content_change()
                                },
                            );
                        }
                        Err(error) => {
                            let rollback_error = response
                                .rewrite_transactions
                                .clone()
                                .into_iter()
                                .rev()
                                .find_map(|transaction| {
                                    self.rewrite_source.rollback(transaction).err()
                                });
                            self.chat.error = Some(match (self.language, rollback_error) {
                                (
                                    crate::preferences::AppLanguage::SimplifiedChinese,
                                    Some(rollback_error),
                                ) => format!(
                                    "应用正文改写失败：{error}；回滚也失败：{rollback_error}"
                                ),
                                (
                                    crate::preferences::AppLanguage::English,
                                    Some(rollback_error),
                                ) => {
                                    format!(
                                        "Failed to apply the content rewrite: {error}; rollback also failed: {rollback_error}"
                                    )
                                }
                                (crate::preferences::AppLanguage::SimplifiedChinese, None) => {
                                    format!("应用正文改写失败：{error}")
                                }
                                (crate::preferences::AppLanguage::English, None) => {
                                    format!("Failed to apply the content rewrite: {error}")
                                }
                            });
                            return;
                        }
                    }
                }
                self.chat.messages.push(ChatTurn {
                    role: ChatRole::Assistant,
                    content: response.content,
                    display_content: None,
                });
                if !response.annotation_actions.is_empty() {
                    self.chat.pending_annotation_actions = response.annotation_actions;
                }
                self.chat.error = None;
            }
            Err(error) => {
                crate::diagnostics::log(
                    "chat.complete.error",
                    &[
                        crate::diagnostics::Field::U64("id", message.id),
                        crate::diagnostics::Field::Usize("error_chars", error.chars().count()),
                    ],
                );
                self.chat.error = Some(error);
            }
        }
    }

    pub(crate) fn update_chat_stream(&mut self, message: ChatStreamMessage) {
        let Some(streaming) = self.chat.streaming.as_mut() else {
            return;
        };
        if streaming.task_id != message.id {
            return;
        }
        let first_content = streaming.content.is_empty() && !message.content.is_empty();
        streaming.content = message.content;
        if first_content {
            crate::diagnostics::log(
                "chat.stream.first",
                &[crate::diagnostics::Field::U64("id", message.id)],
            );
        }
    }

    pub(super) fn clear_chat(&mut self) {
        if !self.chat.task.is_pending() {
            self.chat.messages.clear();
            self.chat.pending_annotation_actions.clear();
            self.chat.error = None;
        }
    }

    pub(super) fn confirm_chat_annotation_actions(&mut self) {
        let actions = std::mem::take(&mut self.chat.pending_annotation_actions);
        let mut error = None;
        for action in actions {
            let result = match action {
                ChatAnnotationAction::Create(annotation) => {
                    self.highlight_store.insert(&annotation).map(|()| true)
                }
                ChatAnnotationAction::Update(annotation) => {
                    self.highlight_store.update(&annotation)
                }
                ChatAnnotationAction::Delete { annotation_id } => {
                    self.highlight_store.remove(&annotation_id)
                }
            };
            if let Err(action_error) = result {
                error = Some(action_error.to_string());
                break;
            }
        }
        self.highlights = self.highlight_store.for_book(&self.book_id);
        self.selected_highlight_id = None;
        self.bump_scene_revision();
        self.chat.error = error.map(|error| {
            format!(
                "{}: {error}",
                self.language.text(
                    "应用 AI 批注操作失败",
                    "Failed to apply AI annotation actions"
                )
            )
        });
    }

    pub(super) fn cancel_chat_annotation_actions(&mut self) {
        self.chat.pending_annotation_actions.clear();
    }

    pub(super) fn toggle_translation(&mut self) {
        self.cancel_text_selection();
        self.translation.clear_error();
        if self.translation.enabled {
            self.translation.enabled = false;
            self.translation.task.cancel();
            self.translation.toc_task.cancel();
            let was_rendering = self.translation.render_enabled;
            if !self.set_translation_rendering(false) {
                return;
            }
            if was_rendering {
                self.refresh_translation_view();
            }
            return;
        }

        if let Err(error) = self.plugin_settings.translation_endpoint() {
            self.translation.show_error(error.clone(), Instant::now());
            self.error = Some(error);
            return;
        }
        self.translation_source.set_fixed_page_replacement_only(
            self.format == rebook_formats::BookFormat::Pdf
                && self.pdf_ocr.mode == PdfOcrViewMode::Original,
        );
        if let Err(error) = self
            .translation_source
            .set_mode(self.plugin_settings.translation_mode)
        {
            self.translation.show_error(error, Instant::now());
            return;
        }
        self.translation.enabled = true;
        if self.set_translation_rendering(true) {
            self.refresh_translation_view();
        }
        self.queue_visible_section_translation();
        self.queue_toc_translation();
    }

    pub(super) fn dismiss_translation_notice(&mut self) {
        self.translation.clear_error();
    }

    pub(super) fn queue_visible_section_translation(&mut self) {
        if !self.translation.enabled || self.translation.task.is_pending() {
            return;
        }
        let visible = match self.current_translation_ranges() {
            Ok(visible) => visible,
            Err(error) => {
                self.translation.show_error(
                    format!(
                        "{}: {error}",
                        self.language
                            .text("读取当前页面失败", "Failed to inspect the current page")
                    ),
                    Instant::now(),
                );
                return;
            }
        };
        let candidate = visible.into_iter().find_map(|(section_index, ranges)| {
            match self
                .translation_source
                .untranslated_blocks_for_ranges(section_index, &ranges)
            {
                Ok(blocks) if blocks.is_empty() => None,
                Ok(blocks) => Some(Ok((section_index, blocks))),
                Err(error) => Some(Err(error)),
            }
        });
        let Some(candidate) = candidate else { return };
        let (section_index, blocks) = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                self.translation.show_error(error, Instant::now());
                return;
            }
        };
        self.translation.clear_error();
        let mut settings = self.plugin_settings.clone();
        settings.target_language =
            settings.resolved_target_language(self.language.translation_target());
        self.translation.task.begin(TranslationTask {
            section_index,
            settings,
            blocks,
        });
    }

    pub(super) fn queue_toc_translation(&mut self) {
        if !self.translation.enabled
            || !self.plugin_settings.translate_toc
            || self.translation.toc_task.is_pending()
            || !self.translation.toc_labels.is_empty()
        {
            return;
        }
        let mut toc_ids = Vec::new();
        let mut blocks = Vec::new();
        for row in self.reader.toc_items() {
            if row.label.trim().is_empty() {
                continue;
            }
            let block_index = toc_ids.len();
            toc_ids.push(row.id.clone());
            blocks.push(TranslationBlockInput {
                block_index,
                segment_index: None,
                text: row.label.clone(),
            });
        }
        if blocks.is_empty() {
            return;
        }
        let mut settings = self.plugin_settings.clone();
        settings.target_language =
            settings.resolved_target_language(self.language.translation_target());
        self.translation.toc_task.begin(TocTranslationTask {
            toc_ids,
            settings,
            blocks,
        });
    }

    pub(crate) fn complete_translation(&mut self, message: TranslationTaskMessage) {
        match message {
            TranslationTaskMessage::Batch { id, translations } => {
                let Some(section_index) = self
                    .translation
                    .task
                    .in_flight(id)
                    .map(|request| request.section_index)
                else {
                    return;
                };
                if let Err(error) = self
                    .translation_source
                    .store_batch(section_index, &translations)
                {
                    self.translation.show_error(error, Instant::now());
                    return;
                }
                self.translation.clear_error();
                self.refresh_translation_view();
            }
            TranslationTaskMessage::Complete(message) => {
                let Some(_request) = self.translation.task.complete(message.id) else {
                    return;
                };
                match message.result {
                    Ok(()) => {
                        self.translation.clear_error();
                        self.queue_visible_section_translation();
                    }
                    Err(error) => {
                        self.error = Some(format!(
                            "{}: {error}",
                            self.language
                                .text("翻译正文失败", "Failed to translate book content")
                        ));
                        self.translation.show_error(error, Instant::now());
                    }
                }
            }
        }
    }

    pub(crate) fn complete_toc_translation(&mut self, message: TocTranslationTaskMessage) {
        let Some(request) = self.translation.toc_task.complete(message.id) else {
            return;
        };
        match message.result {
            Ok(translations) => {
                self.translation.toc_labels =
                    translated_toc_labels(&request.toc_ids, &translations);
                self.translation.clear_error();
            }
            Err(error) => {
                self.error = Some(format!(
                    "{}: {error}",
                    self.language
                        .text("翻译目录失败", "Failed to translate table of contents")
                ));
                self.translation.show_error(error, Instant::now());
            }
        }
    }

    pub(super) fn refresh_translation_view(&mut self) {
        let preserve_scroll_offset = self.is_scroll_mode() && self.scroll_viewport.is_some();
        match self.reader.refresh_source() {
            Ok(snapshot) => {
                self.apply_snapshot(
                    snapshot,
                    SnapshotEffects {
                        marks: MarkRetention::ClearSelectedHighlight,
                        ..SnapshotEffects::static_content_change()
                    },
                );
                if preserve_scroll_offset {
                    self.scroll_target_position = None;
                    self.scroll_viewport = None;
                }
            }
            Err(error) => self.translation.show_error(
                format!(
                    "{}: {error}",
                    self.language
                        .text("刷新翻译正文失败", "Failed to refresh translated content")
                ),
                Instant::now(),
            ),
        }
    }

    fn current_translation_ranges(
        &mut self,
    ) -> Result<Vec<(usize, Vec<SourceRange>)>, rebook_reader::ReaderError> {
        let fragments = if self.is_scroll_mode() {
            let positions = self
                .scroll_section
                .as_ref()
                .zip(self.scroll_viewport)
                .map(|(layout, viewport)| layout.visible_pages(viewport));
            if let Some(positions) = positions {
                self.reader.visible_text_fragments_for_pages(&positions)?
            } else {
                self.reader.current_visible_text_fragments()?
            }
        } else {
            self.reader.current_visible_text_fragments()?
        };
        let mut sections = Vec::<(usize, Vec<SourceRange>)>::new();
        for fragment in fragments {
            let section_index = fragment.position.section_index;
            if let Some((_, ranges)) = sections
                .iter_mut()
                .find(|(candidate, _)| *candidate == section_index)
            {
                ranges.push(fragment.range);
            } else {
                sections.push((section_index, vec![fragment.range]));
            }
        }
        Ok(sections)
    }

    fn set_translation_rendering(&mut self, enabled: bool) -> bool {
        if self.translation.render_enabled == enabled {
            return true;
        }
        if let Err(error) = self.translation_source.set_enabled(enabled) {
            self.translation.show_error(error, Instant::now());
            return false;
        }
        self.translation.render_enabled = enabled;
        true
    }
}

fn paragraph_reference(
    section_index: usize,
    paragraph_index: usize,
    section_title: &str,
    node: &str,
    part_index: usize,
    text: &str,
    english: bool,
) -> ChatReference {
    let label = clip_chat_reference_text(text, 32);
    let excerpt = clip_chat_reference_text(text, 220);
    ChatReference {
        id: format!("paragraph:{section_index}:{node}:{part_index}"),
        kind: ChatReferenceKind::Paragraph,
        label,
        description: if english {
            format!("Paragraph {paragraph_index} · {section_title}")
        } else {
            format!("段落 {paragraph_index} · {section_title}")
        },
        link: chat_citation_link(section_index, Some(node)),
        excerpt: Some(excerpt),
    }
}

fn log_completed_chat(id: u64, response: &ChatResponse) {
    let summary = super::chat_markdown::diagnostic_summary(&response.content);
    crate::diagnostics::log(
        "chat.complete.ok",
        &[
            crate::diagnostics::Field::U64("id", id),
            crate::diagnostics::Field::Usize("response_chars", response.content.chars().count()),
            crate::diagnostics::Field::Usize("response_lines", response.content.lines().count()),
            crate::diagnostics::Field::Usize("rewrites", response.rewrites.len()),
            crate::diagnostics::Field::Usize("render_blocks", summary.render_blocks),
            crate::diagnostics::Field::Usize("plain_fences", summary.plain_fenced_code),
            crate::diagnostics::Field::Usize("tables", summary.tables),
            crate::diagnostics::Field::Usize("emoji_like", summary.emoji_like),
            crate::diagnostics::Field::Usize("svg_previews", summary.svg_previews),
            crate::diagnostics::Field::Usize("mermaid_previews", summary.mermaid_previews),
            crate::diagnostics::Field::Usize("formulas", summary.formulas),
            crate::diagnostics::Field::Usize("citations", summary.citations),
        ],
    );
}

fn selection_reference(
    source: &dyn BookSource,
    ranges: &[SourceRange],
    selected_text: &str,
    english: bool,
) -> Option<ChatReference> {
    let range = ranges.first()?;
    let section_index = source
        .book()
        .sections
        .iter()
        .position(|section| section.id == range.start.spine)?;
    let section = source.parse_section(section_index).ok()?;
    let title = section_title(source, section_index, &section.blocks);
    let paragraph = section.blocks.iter().find_map(|block| {
        let source_range = block_source_range(block)?;
        (source_range.start.node == range.start.node).then(|| block_text(block))
    });
    Some(ChatReference {
        id: format!("selection:{section_index}:{}", range.start.node),
        kind: ChatReferenceKind::Paragraph,
        label: clip_chat_reference_text(selected_text, 32),
        description: if english {
            format!("Selected paragraph · {title}")
        } else {
            format!("选中段落 · {title}")
        },
        link: chat_citation_link(section_index, Some(&range.start.node)),
        excerpt: paragraph
            .filter(|text| !text.trim().is_empty())
            .map(|text| clip_chat_reference_text(&text, 500)),
    })
}

fn source_range_for_node(
    source: &dyn BookSource,
    section_index: usize,
    node: &str,
) -> Option<SourceRange> {
    source
        .parse_section(section_index)
        .ok()?
        .blocks
        .iter()
        .find_map(|block| {
            let range = block_source_range(block)?;
            (range.start.node == node).then(|| range.clone())
        })
}

fn block_source_range(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(block) => block.source.as_ref(),
        Block::Table(block) => block.source.as_ref(),
        Block::Image(block) => block.source.as_ref(),
        Block::Separator | Block::PageBreak => None,
    }
}

fn block_text(block: &Block) -> String {
    match block {
        Block::Text(block) => block
            .content
            .iter()
            .map(|inline| match inline {
                Inline::Text(run) => run.text.as_str(),
                Inline::Math(run) => run.latex.as_str(),
                Inline::Break => "\n",
            })
            .collect(),
        Block::Table(table) => table
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| {
                        cell.text
                            .content
                            .iter()
                            .map(|inline| match inline {
                                Inline::Text(run) => run.text.as_str(),
                                Inline::Math(run) => run.latex.as_str(),
                                Inline::Break => "\n",
                            })
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Block::Image(block) => block
            .text_layer
            .as_ref()
            .map_or_else(|| block.alt.clone(), |layer| layer.text.clone()),
        Block::Separator | Block::PageBreak => String::new(),
    }
}

type VisibleChatParagraph = (usize, String, usize, String);

fn visible_chat_paragraphs(fragments: Vec<ReaderVisibleTextFragment>) -> Vec<VisibleChatParagraph> {
    let mut paragraphs = Vec::<VisibleChatParagraph>::new();
    for fragment in fragments {
        for (part_index, part) in fragment.text.split("\n\n").enumerate() {
            let text = normalize_chat_reference_text(part);
            if text.chars().count() < 2 {
                continue;
            }
            let section_index = fragment.position.section_index;
            let node = fragment.range.start.node.clone();
            if let Some((_, _, _, combined)) = paragraphs.iter_mut().find(
                |(candidate_section, candidate_node, candidate_part, _)| {
                    *candidate_section == section_index
                        && *candidate_node == node
                        && *candidate_part == part_index
                },
            ) {
                combined.push(' ');
                combined.push_str(&text);
            } else {
                paragraphs.push((section_index, node, part_index, text));
            }
        }
    }
    paragraphs
}

fn normalize_chat_reference_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip_chat_reference_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut clipped = value.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}

fn semantic_result_excerpt(value: &str) -> String {
    const MAX_CHARS: usize = 140;
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let mut excerpt = normalized.chars().take(MAX_CHARS).collect::<String>();
    excerpt.push('…');
    excerpt
}

fn semantic_index_percentage(completed: usize, total: usize) -> usize {
    if total == 0 {
        return 100;
    }
    completed.min(total).saturating_mul(100) / total
}

fn translated_toc_labels(
    toc_ids: &[String],
    translations: &[crate::plugins::BlockTranslation],
) -> std::collections::HashMap<String, String> {
    translations
        .iter()
        .filter_map(|translation| {
            let id = toc_ids.get(translation.block_index)?;
            (!translation.text.trim().is_empty()).then(|| (id.clone(), translation.text.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::BlockTranslation;

    #[test]
    fn toc_translations_are_mapped_by_their_stable_row_ids() {
        let ids = vec!["cover".into(), "chapter-1".into()];
        let labels = translated_toc_labels(
            &ids,
            &[
                BlockTranslation {
                    block_index: 1,
                    segment_index: None,
                    text: "第一章".into(),
                },
                BlockTranslation {
                    block_index: 99,
                    segment_index: None,
                    text: "ignored".into(),
                },
            ],
        );

        assert_eq!(labels.len(), 1);
        assert_eq!(labels.get("chapter-1").map(String::as_str), Some("第一章"));
    }

    #[test]
    fn semantic_results_use_a_compact_whitespace_normalized_excerpt() {
        let source = format!("  first\n\n{}  ", "内容".repeat(80));

        let excerpt = semantic_result_excerpt(&source);

        assert_eq!(excerpt.chars().count(), 141);
        assert!(excerpt.starts_with("first 内容"));
        assert!(excerpt.ends_with('…'));
    }

    #[test]
    fn semantic_index_progress_is_a_bounded_percentage() {
        assert_eq!(semantic_index_percentage(0, 10), 0);
        assert_eq!(semantic_index_percentage(3, 10), 30);
        assert_eq!(semantic_index_percentage(10, 10), 100);
        assert_eq!(semantic_index_percentage(12, 10), 100);
        assert_eq!(semantic_index_percentage(0, 0), 100);
    }
}
