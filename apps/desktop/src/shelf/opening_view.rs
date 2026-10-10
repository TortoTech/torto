use super::*;
use rebook_publication::{PublicationUrl, TocEntry};

pub(crate) enum Action {
    Back,
    Retry,
    Navigate(PublicationUrl),
}

pub(crate) struct OpeningShell {
    pub book: LibraryBook,
    pub toc: Option<Arc<[TocEntry]>>,
    pub error: Option<String>,
    pub sidebar: crate::reader::chrome::SidebarState,
    pub mode: crate::preferences::ReadingMode,
    requested_mode: crate::preferences::ReadingMode,
    chrome_ready: bool,
    sidebar_touched: bool,
    pub target: Option<PublicationUrl>,
    pub header_title: Option<String>,
    pub pdf_toolbar: crate::reader::chrome::PdfToolbarState,
    pub shell_presented: bool,
    pub toc_presented: bool,
    pub drawn: bool,
    pub started: Instant,
    expanded: std::collections::HashSet<Vec<usize>>,
}

impl OpeningShell {
    #[cfg(test)]
    pub(crate) fn expand_toc_path(&mut self, path: &[usize]) {
        self.expanded.insert(path.to_vec());
    }
    pub fn new(
        book: LibraryBook,
        started: Instant,
        mode: crate::preferences::ReadingMode,
        classic: crate::reader::chrome::SidebarState,
    ) -> Self {
        Self {
            chrome_ready: Self::mode_is_known(&book, mode),
            expanded: Default::default(),
            toc: book.cached_toc.clone(),
            book,
            error: None,
            sidebar: classic.for_mode(mode),
            mode,
            requested_mode: mode,
            sidebar_touched: false,
            target: None,
            header_title: None,
            pdf_toolbar: Default::default(),
            shell_presented: false,
            toc_presented: false,
            drawn: false,
            started,
        }
    }

    pub fn set_mode(
        &mut self,
        mode: crate::preferences::ReadingMode,
        classic: crate::reader::chrome::SidebarState,
    ) {
        if self.requested_mode != mode {
            self.requested_mode = mode;
            self.mode = mode;
            self.chrome_ready = Self::mode_is_known(&self.book, mode);
            self.sidebar = classic.for_mode(mode);
            self.sidebar_touched = false;
        }
    }

    pub fn set_effective_mode(
        &mut self,
        mode: crate::preferences::ReadingMode,
        classic: crate::reader::chrome::SidebarState,
    ) {
        self.chrome_ready = true;
        if self.mode != mode {
            let open = self.sidebar.open;
            self.mode = mode;
            self.sidebar = classic.for_mode(mode);
            if self.sidebar_touched {
                self.sidebar.open = open;
            }
        }
    }

    fn mode_is_known(book: &LibraryBook, mode: crate::preferences::ReadingMode) -> bool {
        // Original PDF pages fall back from Focus to Classic. Wait for the worker
        // to resolve that choice before exposing a header with provisional geometry.
        mode != crate::preferences::ReadingMode::Focus
            || rebook_formats::BookFormat::from_file_name(&book.file_name)
                != Some(rebook_formats::BookFormat::Pdf)
    }

    pub fn chrome_drawn(&self) -> bool {
        self.drawn && (self.chrome_ready || self.error.is_some())
    }

    pub fn ui(
        &mut self,
        root: &mut egui::Ui,
        covers: &mut covers::CoverCache,
        language: AppLanguage,
        shortcuts: &crate::preferences::ShortcutPreferences,
        blocked: bool,
    ) -> Option<Action> {
        // The paper frame can start GPU prewarming even while header geometry
        // awaits PDF mode selection. Only report chrome once it is actually drawn.
        self.drawn = true;
        let mut action = None;
        if !blocked
            && !root.ctx().text_edit_focused()
            && root
                .ctx()
                .input_mut(|input| input.consume_shortcut(&shortcuts.return_to_shelf))
        {
            action = Some(Action::Back);
        }
        if self.chrome_drawn()
            && !blocked
            && !root.ctx().text_edit_focused()
            && root
                .ctx()
                .input_mut(|input| input.consume_shortcut(&shortcuts.toggle_left_sidebar))
        {
            self.sidebar.open = !self.sidebar.open;
            self.sidebar_touched = true;
            root.ctx().request_repaint();
        }
        let mut style = rebook_layout::ReaderStyle::default();
        crate::reader::apply_theme_colors(&mut style, crate::ui::theme());
        let paper = egui::Color32::from_rgba_unmultiplied(
            style.background.red,
            style.background.green,
            style.background.blue,
            style.background.alpha,
        );
        let screen = root.ctx().content_rect();
        let format = rebook_formats::BookFormat::from_file_name(&self.book.file_name);
        #[cfg(target_os = "windows")]
        {
            crate::app::window_chrome::header(
                root.ctx(),
                egui::Rect::from_min_size(screen.min, Vec2::new(screen.width(), 44.0)),
            );
            crate::app::window_chrome::set_background(root.ctx(), paper);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(paper))
            .show(root, |ui| {
                if !self.chrome_drawn() {
                    return;
                }
                let (header, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 44.0),
                    egui::Sense::hover(),
                );
                let header = egui::Rect::from_min_max(
                    header.min + Vec2::new(self.sidebar.reserved_width(self.mode), 0.0),
                    header.max,
                );
                let title_center = header.center().x;
                #[cfg(target_os = "windows")]
                let header = egui::Rect::from_min_max(
                    header.min,
                    egui::pos2(
                        (header.right()
                            - crate::app::window_chrome::reserve_width(ui.ctx())
                            - 44.0)
                            .max(header.left() + 1.0),
                        header.bottom(),
                    ),
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(header)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.set_clip_rect(ui.clip_rect().intersect(header));
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if blocked {
                            ui.disable();
                        }
                        ui.add_space(8.0);
                        if icon_button(ui, Icon::PanelLeft)
                            .on_hover_text(language.text("目录", "Contents"))
                            .clicked()
                        {
                            self.sidebar.open = !self.sidebar.open;
                            self.sidebar_touched = true;
                        }
                        if icon_button(ui, Icon::Library)
                            .on_hover_text(language.text("返回书架", "Back to library"))
                            .clicked()
                        {
                            action = Some(Action::Back);
                        }
                        ui.add_enabled_ui(false, |ui| {
                            icon_button(ui, Icon::Languages).on_disabled_hover_text(language.text(
                                "正文加载后可使用翻译",
                                "Translation is available after the text loads",
                            ));
                        });
                        if format == Some(rebook_formats::BookFormat::Pdf) {
                            ui.add_enabled_ui(false, |ui| {
                                icon_button(ui, Icon::BookOpen).on_disabled_hover_text(
                                    language.text(
                                        "正文加载后可生成 PDF 文字",
                                        "PDF text generation is available after the text loads",
                                    ),
                                );
                            });
                            crate::reader::chrome::pdf_text_switch(
                                ui,
                                language,
                                self.pdf_toolbar,
                                false,
                                false,
                            );
                        }
                    },
                );
                // Keep the same control positions while reader-dependent actions prepare.
                #[cfg(target_os = "windows")]
                let menu_left = header.right();
                #[cfg(not(target_os = "windows"))]
                let menu_left = header.right() - 44.0;
                for (glyph, left, hint) in [
                    (
                        Icon::PanelRight,
                        crate::reader::chrome::right_sidebar_button_left(screen.right(), menu_left),
                        language.text(
                            "正文加载后可展开右侧栏",
                            "The right sidebar is available after the text loads",
                        ),
                    ),
                    (
                        Icon::Menu,
                        menu_left,
                        language.text(
                            "正文加载后可打开菜单",
                            "The menu is available after the text loads",
                        ),
                    ),
                ] {
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(left, header.top() + 6.0),
                        Vec2::splat(32.0),
                    );
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        ui.set_clip_rect(ui.clip_rect().intersect(rect));
                        ui.add_enabled_ui(false, |ui| {
                            icon_button(ui, glyph).on_disabled_hover_text(hint);
                        });
                    });
                }
                if let Some(title) = &self.header_title {
                    crate::reader::paint_toolbar_title(
                        ui,
                        header,
                        title_center,
                        title,
                        3.0 + crate::reader::chrome::pdf_toolbar_control_count(format),
                    );
                }
                if let Some(error) = &self.error {
                    ui.add_space(24.0);
                    ui.horizontal(|ui| {
                        ui.add_space(self.sidebar.reserved_width(self.mode) + 24.0);
                        ui.vertical(|ui| {
                            ui.colored_label(palette().error_text, error);
                            if ui
                                .add_enabled(
                                    !blocked,
                                    egui::Button::new(language.text("重试", "Retry")),
                                )
                                .clicked()
                            {
                                action = Some(Action::Retry);
                            }
                        });
                    });
                }
            });
        if self.chrome_drawn() && self.sidebar.open {
            let pinned = self.sidebar.reserved_width(self.mode) > 0.0;
            let sidebar = egui::Rect::from_min_max(
                screen.min,
                egui::pos2(
                    (screen.left() + self.sidebar.width).min(screen.right()),
                    screen.bottom(),
                ),
            );
            // Its size is known: a bounded child paints immediately, whereas a
            // new Area silently hides its first frame for an automatic sizing pass.
            let mut sidebar_ui = root.new_child(
                egui::UiBuilder::new()
                    .id_salt("opening-reader-sidebar")
                    .max_rect(sidebar),
            );
            sidebar_ui.set_clip_rect(sidebar_ui.clip_rect().intersect(sidebar));
            {
                let ui = &mut sidebar_ui;
                ui.set_min_size(sidebar.size());
                ui.set_max_size(sidebar.size());
                let frame = crate::reader::chrome::sidebar_frame(!pinned);
                let inset = frame.total_margin().sum();
                frame.show(ui, |ui| {
                    ui.set_min_size(sidebar.size() - inset);
                    ui.set_max_width((sidebar.width() - inset.x).max(1.0));
                    if blocked {
                        ui.disable();
                    }
                    if let Some(crate::reader::chrome::SidebarAction::TogglePin) =
                        crate::reader::chrome::sidebar_toolbar(
                            ui,
                            language,
                            self.mode,
                            self.sidebar.pinned,
                            crate::reader::SidebarTab::Toc,
                            false,
                        )
                    {
                        self.sidebar.pinned = !self.sidebar.pinned;
                    }
                    let texture = self.book.cover_bytes.as_deref().and_then(|bytes| {
                        covers.texture_sized(
                            ui.ctx(),
                            &self.book.id,
                            bytes,
                            crate::reader::chrome::SIDEBAR_COVER_SIZE,
                        )
                    });
                    let format = rebook_formats::BookFormat::from_file_name(&self.book.file_name);
                    crate::reader::chrome::sidebar_book_summary(
                        ui,
                        &self.book.title,
                        &self.book.authors,
                        format.map_or("", |format| format.label()),
                        texture.as_ref(),
                    );
                    ui.separator();
                    ui.add_space(4.0);
                    if let Some(toc) = &self.toc {
                        let mut rows = Vec::new();
                        collect_toc_rows(toc, &self.expanded, &mut Vec::new(), &mut rows);
                        egui::ScrollArea::vertical()
                            .id_salt("opening-reader-toc")
                            .auto_shrink([false, false])
                            .max_height(ui.available_height().max(1.0))
                            .show_rows(
                                ui,
                                crate::reader::chrome::TOC_ROW_HEIGHT,
                                rows.len(),
                                |ui, visible| {
                                    ui.set_width((ui.available_width() - 12.0).max(1.0));
                                    for row in &rows[visible] {
                                        ui.push_id(&row.path, |ui| {
                                            let event = crate::reader::chrome::toc_row(
                                                ui,
                                                crate::reader::chrome::TocRowAppearance {
                                                    id: &format!("{:?}", row.path),
                                                    label: &row.entry.label,
                                                    depth: row.path.len() - 1,
                                                    has_children: !row.entry.children.is_empty(),
                                                    expanded: self.expanded.contains(&row.path),
                                                    selected: row.entry.href.as_ref().is_some_and(
                                                        |href| Some(href) == self.target.as_ref(),
                                                    ),
                                                    keyboard_focused: false,
                                                },
                                                language,
                                            );
                                            match event {
                                                Some(
                                                    crate::reader::chrome::TocRowAction::Navigate,
                                                ) if row.entry.href.is_some() => {
                                                    action = Some(Action::Navigate(
                                                        row.entry.href.clone().unwrap(),
                                                    ));
                                                }
                                                Some(_) if !row.entry.children.is_empty() => {
                                                    if !self.expanded.remove(&row.path) {
                                                        self.expanded.insert(row.path.clone());
                                                    }
                                                }
                                                _ => {}
                                            }
                                        });
                                    }
                                },
                            );
                    }
                });
            }
        }
        action
    }
}

struct TocRow<'a> {
    entry: &'a TocEntry,
    path: Vec<usize>,
}

fn collect_toc_rows<'a>(
    entries: &'a [TocEntry],
    expanded: &std::collections::HashSet<Vec<usize>>,
    path: &mut Vec<usize>,
    rows: &mut Vec<TocRow<'a>>,
) {
    for (index, entry) in entries.iter().enumerate() {
        path.push(index);
        rows.push(TocRow {
            entry,
            path: path.clone(),
        });
        if expanded.contains(path) {
            collect_toc_rows(&entry.children, expanded, path, rows);
        }
        path.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(
        mode: crate::preferences::ReadingMode,
        classic: crate::reader::chrome::SidebarState,
    ) -> OpeningShell {
        OpeningShell::new(
            LibraryBook {
                id: "book".into(),
                title: "Book".into(),
                authors: vec![],
                file_name: "book.epub".into(),
                path: PathBuf::from("book.epub"),
                cover_bytes: Some(vec![1, 2, 3]),
                added_at: 0,
                cached_toc: Some(
                    vec![TocEntry {
                        label: "Lazy chapter".into(),
                        href: None,
                        children: vec![],
                    }]
                    .into(),
                ),
            },
            Instant::now(),
            mode,
            classic,
        )
    }

    #[test]
    fn opening_toc_and_header_appear_together_without_a_provisional_pdf_header() {
        use crate::preferences::ReadingMode;
        for (pdf, requested, effective, open) in [
            (false, ReadingMode::Classic, ReadingMode::Classic, true),
            (false, ReadingMode::Classic, ReadingMode::Classic, false),
            (true, ReadingMode::Focus, ReadingMode::Classic, true),
            (true, ReadingMode::Focus, ReadingMode::Classic, false),
            (true, ReadingMode::Focus, ReadingMode::Focus, true),
        ] {
            let classic = crate::reader::chrome::SidebarState {
                open,
                width: 310.0,
                ..Default::default()
            };
            let mut book = shell(requested, classic).book;
            if pdf {
                book.file_name = "book.PDF".into();
            }
            let mut shell = OpeningShell::new(book, Instant::now(), requested, classic);
            shell.header_title = Some("Current chapter".into());
            let ctx = egui::Context::default();
            let mut covers = covers::CoverCache::default();
            let mut centers = Vec::new();
            for frame in 0..3 {
                if frame == 1 {
                    shell.set_effective_mode(effective, classic);
                }
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1200.0, 800.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        #[cfg(target_os = "windows")]
                        crate::app::window_chrome::begin_frame(ui.ctx());
                        covers.begin_frame(ui.ctx());
                        shell.ui(
                            ui,
                            &mut covers,
                            AppLanguage::English,
                            &Default::default(),
                            false,
                        );
                        assert_eq!(
                            ui.max_rect().width(),
                            1200.0,
                            "sidebar must not change the viewport used by opening workers"
                        );
                    },
                );
                let header = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == "Current chapter" => {
                        Some(text)
                    }
                    _ => None,
                });
                let toc = output.shapes.iter().find(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Lazy chapter"));
                let pending_mode = pdf && frame == 0;
                assert_eq!(
                    header.is_some(),
                    !pending_mode,
                    "frame={frame} pdf={pdf} requested={requested:?}"
                );
                assert!(
                    shell.drawn,
                    "the first paper frame starts background GPU prewarming"
                );
                assert_eq!(shell.chrome_drawn(), !pending_mode);
                assert_eq!(
                    toc.is_some(),
                    !pending_mode && effective == ReadingMode::Classic && open,
                    "directory and header must both paint in their first visible frame"
                );
                if let Some(header) = header {
                    let center = header.pos.x + header.galley.size().x * 0.5;
                    assert!(
                        (center - (1200.0 + shell.sidebar.reserved_width(effective)) * 0.5).abs()
                            < 1.0
                    );
                    centers.push(center);
                }
                if let Some(toc) = toc {
                    assert!(toc.clip_rect.right() <= classic.width);
                    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == palette().surface && rect.rect.top() == 0.0)), "pinned directory covers the same full-height column as the loaded reader");
                }
                if pending_mode {
                    assert_eq!(covers.pending_decode_count(), 0);
                }
                output.textures_delta.clear();
            }
            assert!(
                centers
                    .windows(2)
                    .all(|pair| (pair[0] - pair[1]).abs() < 1.0)
            );
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn opening_header_title_is_centered_from_first_frame_and_clear_of_controls() {
        use crate::app::window_chrome;
        use crate::preferences::ReadingMode;
        for width in [720.0, 1200.0, 2560.0] {
            for fullscreen in [false, true] {
                for (mode, sidebar_open) in [
                    (ReadingMode::Focus, false),
                    (ReadingMode::Classic, false),
                    (ReadingMode::Classic, true),
                ] {
                    let classic = crate::reader::chrome::SidebarState {
                        open: sidebar_open,
                        ..Default::default()
                    };
                    let mut shell = shell(mode, classic);
                    for title in ["Thinking in Systems", "A long book title that must remain centered and truncate before reaching the reader controls or native window buttons"].map(str::to_owned) {
                        shell.header_title = Some(title);
                        let ctx = egui::Context::default();
                        window_chrome::set_state(&ctx, window_chrome::WindowState { fullscreen, ..Default::default() });
                        let mut covers = covers::CoverCache::default();
                        for frame in 0..2 {
                            let mut output = ctx.run_ui(egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0))),
                                ..Default::default()
                            }, |ui| {
                                window_chrome::begin_frame(ui.ctx());
                                covers.begin_frame(ui.ctx());
                                shell.ui(ui, &mut covers, AppLanguage::English, &Default::default(), false);
                                window_chrome::paint_controls(ui.ctx());
                            });
                            let left = shell.sidebar.reserved_width(mode);
                            let right = width - window_chrome::reserve_width(&ctx) - 44.0;
                            let expected_center = (left + width) * 0.5;
                            let (shape, title) = output.shapes.iter().find_map(|shape| match &shape.shape {
                                egui::Shape::Text(text) if text.pos.y < 44.0 && Some(text.galley.text()) == shell.header_title.as_deref() => Some((shape, text)),
                                _ => None,
                            }).expect("opening header title is painted in the first frame");
                            let center = title.pos.x + title.galley.size().x * 0.5;
                            assert!((center - expected_center).abs() < 1.0, "frame={frame} width={width} mode={mode:?} title center={center} expected={expected_center}");
                            assert!(shape.clip_rect.left() >= left && shape.clip_rect.right() <= right);
                            let geometry = window_chrome::geometry(&ctx);
                            for control_center_x in [left + 24.0, left + 56.0, left + 88.0, right - 16.0, right + 16.0] {
                                assert!(geometry.excluded.iter().any(|rect| (rect.center().x - control_center_x).abs() < 1.0 && (rect.center().y - 22.0).abs() < 1.0), "all five loading header controls are present from frame={frame}");
                            }
                            assert!(geometry.excluded.iter().filter(|rect| rect.right() <= right).all(|rect| !rect.intersects(shape.clip_rect)), "title cannot overlap action buttons");
                            output.textures_delta.clear();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_loading_chapter_does_not_show_the_book_name_as_a_chapter() {
        let mut shell = shell(crate::preferences::ReadingMode::Focus, Default::default());
        let mut covers = covers::CoverCache::default();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                shell.ui(
                    ui,
                    &mut covers,
                    AppLanguage::English,
                    &Default::default(),
                    false,
                );
            },
        );
        assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.pos.y < 44.0 && text.galley.text() == shell.book.title)));
        output.textures_delta.clear();
    }

    #[test]
    fn hidden_cached_toc_skips_sidebar_widgets_and_cover_requests_until_shortcut() {
        let mut shell = shell(crate::preferences::ReadingMode::Focus, Default::default());
        let mut covers = covers::CoverCache::default();
        let ctx = egui::Context::default();
        let shortcuts = crate::preferences::ShortcutPreferences::default();
        for (frame, expected) in [(0, false), (1, true), (2, false)] {
            covers.begin_frame(&ctx);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 800.0),
                    )),
                    events: if frame == 0 {
                        vec![]
                    } else {
                        vec![egui::Event::Key {
                            key: shortcuts.toggle_left_sidebar.logical_key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: shortcuts.toggle_left_sidebar.modifiers,
                        }]
                    },
                    ..Default::default()
                },
                |ui| {
                    shell.ui(ui, &mut covers, AppLanguage::default(), &shortcuts, false);
                },
            );
            assert_eq!(shell.sidebar.open, expected);
            assert_eq!(covers.pending_decode_count(), usize::from(expected));
            if !expected {
                assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text().contains("Lazy chapter"))));
            } else {
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text().contains("Lazy chapter"))), "shortcut-opened directory paints immediately");
            }
            output.textures_delta.clear();
        }
    }

    #[test]
    fn classic_restores_closed_state_and_effective_mode_does_not_undo_user_action() {
        use crate::preferences::ReadingMode;
        let classic = crate::reader::chrome::SidebarState {
            open: false,
            pinned: true,
            width: 310.0,
        };
        let mut shell = shell(ReadingMode::Classic, classic);
        assert_eq!(shell.sidebar, classic);
        shell.set_mode(ReadingMode::Focus, classic);
        assert!(!shell.sidebar.open && !shell.sidebar.pinned);
        shell.sidebar.open = true;
        shell.sidebar_touched = true;
        shell.set_effective_mode(ReadingMode::Classic, classic);
        assert!(shell.sidebar.open && shell.sidebar.pinned);
        shell.set_mode(ReadingMode::Focus, classic); // Same requested mode: theme changes keep source fallback.
        assert_eq!(shell.mode, ReadingMode::Classic);
        shell.set_mode(ReadingMode::Classic, classic);
        assert_eq!(shell.sidebar, classic);
    }
    #[test]
    fn large_nested_toc_only_exposes_children_of_expanded_groups() {
        let entries = vec![TocEntry {
            label: "Group".into(),
            href: None,
            children: (0..1000)
                .map(|index| TocEntry {
                    label: index.to_string(),
                    href: None,
                    children: vec![],
                })
                .collect(),
        }];
        let mut rows = Vec::new();
        let mut expanded = std::collections::HashSet::new();
        collect_toc_rows(&entries, &expanded, &mut Vec::new(), &mut rows);
        assert_eq!(rows.len(), 1);
        expanded.insert(vec![0]);
        rows.clear();
        collect_toc_rows(&entries, &expanded, &mut Vec::new(), &mut rows);
        assert_eq!(rows.len(), 1001);
        assert_eq!(rows[1000].path, vec![0, 999]);
    }
}
