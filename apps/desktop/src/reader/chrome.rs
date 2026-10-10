//! Device-local reader presentation preferences, independent of book metadata.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::preferences::ReadingMode;

pub(crate) const SIDEBAR_COVER_SIZE: [u32; 2] = [52, 74];

#[derive(Clone, Copy, Debug)]
pub(crate) struct PdfToolbarState {
    pub available: bool,
    pub mode: crate::plugins::PdfOcrViewMode,
}

impl Default for PdfToolbarState {
    fn default() -> Self {
        Self {
            available: false,
            mode: crate::plugins::PdfOcrViewMode::Original,
        }
    }
}

pub(crate) fn pdf_toolbar_control_count(format: Option<rebook_formats::BookFormat>) -> f32 {
    if format == Some(rebook_formats::BookFormat::Pdf) {
        2.0
    } else {
        0.0
    }
}

pub(crate) fn pdf_text_switch(
    ui: &mut egui::Ui,
    language: crate::preferences::AppLanguage,
    state: PdfToolbarState,
    ready: bool,
    switching: bool,
) -> egui::Response {
    let hint = if !ready {
        language.text(
            "正文加载后可切换阅读版式",
            "Reading view is available after the text loads",
        )
    } else if switching {
        language.text("正在切换阅读模式…", "Switching reading view…")
    } else {
        language.text("请先生成 PDF 文字", "Generate PDF text first")
    };
    ui.add_enabled_ui(ready && state.available && !switching, |ui| {
        crate::ui::selectable_icon_button(
            ui,
            crate::ui::Icon::Type,
            state.mode == crate::plugins::PdfOcrViewMode::Reflow,
        )
    })
    .inner
    .on_disabled_hover_text(hint)
    .on_hover_text(if state.mode == crate::plugins::PdfOcrViewMode::Reflow {
        language.text("切换到原始 PDF", "Show original PDF")
    } else {
        language.text("切换到文字版式", "Show text reflow")
    })
}

pub(crate) const TOC_ROW_HEIGHT: f32 = 36.0;

pub(crate) struct TocRowAppearance<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
    pub selected: bool,
    pub keyboard_focused: bool,
}

pub(crate) enum TocRowAction {
    Toggle,
    Navigate,
}

pub(crate) fn toc_row(
    ui: &mut egui::Ui,
    row: TocRowAppearance<'_>,
    language: crate::preferences::AppLanguage,
) -> Option<TocRowAction> {
    use crate::ui::palette;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TOC_ROW_HEIGHT),
        egui::Sense::click(),
    );
    let mut response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if row.selected {
        palette().accent_soft
    } else if row.keyboard_focused || response.hovered() {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill != egui::Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 6.0, fill);
    }
    if row.keyboard_focused {
        ui.painter().rect_stroke(
            rect,
            6.0,
            egui::Stroke::new(1.0, palette().accent.gamma_multiply(0.72)),
            egui::StrokeKind::Inside,
        );
    }
    let depth = u16::try_from(row.depth).unwrap_or(u16::MAX);
    let toggle_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.left() + 2.0 + f32::from(depth) * 12.0,
            rect.top() + 5.0,
        ),
        egui::Vec2::splat(26.0),
    );
    let toggle = row.has_children
        && super::egui_view::toc_toggle_button(
            ui,
            toggle_rect.center(),
            row.id,
            row.expanded,
            row.selected || row.keyboard_focused,
            language.text("折叠", "Collapse"),
            language.text("展开", "Expand"),
        );
    if super::egui_view::paint_toc_label(
        ui,
        super::egui_view::toc_label_rect(rect, toggle_rect),
        row.label,
        row.selected || row.keyboard_focused,
    ) {
        response = response.on_hover_text(row.label);
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), row.label)
    });
    if toggle {
        Some(TocRowAction::Toggle)
    } else if response.clicked() {
        Some(TocRowAction::Navigate)
    } else {
        None
    }
}

pub(crate) fn sidebar_frame(floating: bool) -> egui::Frame {
    #[cfg(target_os = "windows")]
    let margin = egui::Margin {
        top: ((super::egui_view::TOOLBAR_HEIGHT - super::egui_view::TOOLBAR_CONTROL_SIZE) / 2.0)
            as i8
            - i8::from(floating),
        ..egui::Margin::same(8)
    };
    #[cfg(not(target_os = "windows"))]
    let margin = if floating {
        egui::Margin {
            top: ((super::egui_view::TOOLBAR_HEIGHT - super::egui_view::TOOLBAR_CONTROL_SIZE) / 2.0)
                as i8
                - 1,
            ..egui::Margin::same(8)
        }
    } else {
        egui::Margin::same(8)
    };
    let frame = egui::Frame::new()
        .fill(crate::ui::palette().surface)
        .inner_margin(margin);
    if floating {
        frame.stroke(egui::Stroke::new(1.0, crate::ui::palette().border))
    } else {
        frame
    }
}

pub(crate) enum SidebarAction {
    TogglePin,
    Tab(super::SidebarTab),
}

pub(crate) fn sidebar_toolbar(
    ui: &mut egui::Ui,
    language: crate::preferences::AppLanguage,
    mode: ReadingMode,
    pinned: bool,
    tab: super::SidebarTab,
    reader_ready: bool,
) -> Option<SidebarAction> {
    use crate::ui::{Icon, icon_button, selectable_icon_button};
    let mut action = None;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if mode != ReadingMode::Focus
                && icon_button(ui, if pinned { Icon::Pin } else { Icon::PinOff })
                    .on_hover_text(if pinned {
                        language.text("取消固定", "Unpin sidebar")
                    } else {
                        language.text("固定侧栏", "Pin sidebar")
                    })
                    .clicked()
            {
                action = Some(SidebarAction::TogglePin);
            }
            for (target, icon, hint) in [
                (
                    super::SidebarTab::Toc,
                    Icon::ListTree,
                    language.text("目录", "Contents"),
                ),
                (
                    super::SidebarTab::Highlights,
                    Icon::MessageSquareText,
                    language.text("高亮与批注", "Highlights & notes"),
                ),
                (
                    super::SidebarTab::Search,
                    Icon::Search,
                    language.text("搜索", "Search"),
                ),
            ] {
                ui.add_enabled_ui(reader_ready || target == super::SidebarTab::Toc, |ui| {
                    if selectable_icon_button(ui, icon, tab == target)
                        .on_hover_text(hint)
                        .on_disabled_hover_text(
                            language.text("正文加载后可使用", "Available after the text loads"),
                        )
                        .clicked()
                    {
                        action = Some(SidebarAction::Tab(target));
                    }
                });
            }
        });
    });
    action
}

pub(crate) fn sidebar_book_summary(
    ui: &mut egui::Ui,
    title: &str,
    authors: &[String],
    format: &str,
    texture: Option<&egui::TextureHandle>,
) {
    use crate::ui::palette;
    let size = egui::vec2(SIDEBAR_COVER_SIZE[0] as f32, SIDEBAR_COVER_SIZE[1] as f32);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        // Reserve the entire cover slot before decoding. Fitting the image
        // itself must never shrink this row and move the directory underneath.
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        if let Some(texture) = texture {
            let pixels = texture.size_vec2();
            let scale = (size.x / pixels.x.max(1.0)).min(size.y / pixels.y.max(1.0));
            ui.painter().image(
                texture.id(),
                egui::Rect::from_center_size(rect.center(), pixels * scale),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            ui.painter().rect_filled(rect, 5.0, palette().surface_muted);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format,
                egui::FontId::proportional(crate::ui::scaled_font_size(10.0)),
                palette().accent,
            );
        }
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width().max(1.0), size.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.label(egui::RichText::new(title).strong().color(palette().text))
                    .on_hover_text(title);
                if !authors.is_empty() {
                    ui.label(
                        egui::RichText::new(authors.join(" / "))
                            .size(crate::ui::scaled_font_size(12.0))
                            .color(palette().muted),
                    );
                }
            },
        );
    });
    ui.add_space(10.0);
}

/// Match the left action group's contiguous button slots. When the assistant
/// occupies a separate column, keep its toggle inside the reading column.
pub(crate) fn right_sidebar_button_left(content_right: f32, menu_left: f32) -> f32 {
    let size = super::egui_view::TOOLBAR_CONTROL_SIZE;
    (menu_left - size).min(content_right - size - 8.0)
}

/// Resolve a loading header without parsing or laying out chapter content.
/// Prefer an exact anchor, then the chapter entry for the same document.
pub(crate) fn toc_label_for_href<'a>(
    entries: &'a [rebook_publication::TocEntry],
    href: &rebook_publication::PublicationUrl,
) -> Option<&'a str> {
    fn find<'a>(
        entries: &'a [rebook_publication::TocEntry],
        matches: &impl Fn(&rebook_publication::PublicationUrl) -> bool,
    ) -> Option<&'a str> {
        entries.iter().find_map(|entry| {
            find(&entry.children, matches).or_else(|| {
                entry
                    .href
                    .as_ref()
                    .filter(|target| matches(target))
                    .map(|_| entry.label.as_str())
            })
        })
    }
    find(entries, &|target| target == href).or_else(|| {
        find(entries, &|target| {
            target.path() == href.path() && target.fragment().is_none()
        })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SidebarState {
    pub open: bool,
    pub pinned: bool,
    pub width: f32,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            open: true,
            pinned: true,
            width: super::egui_view::SIDEBAR_WIDTH,
        }
    }
}

impl SidebarState {
    pub fn for_mode(self, mode: ReadingMode) -> Self {
        if mode == ReadingMode::Classic {
            self
        } else {
            Self {
                open: false,
                pinned: false,
                ..self
            }
        }
    }

    pub fn constrained(self, viewport_width: f32) -> Self {
        let width = if self.width.is_finite() {
            self.width
        } else {
            Self::default().width
        };
        Self {
            width: width.clamp(220.0, 420.0).min(if self.open && self.pinned {
                (viewport_width - 200.0).max(1.0)
            } else {
                viewport_width.max(1.0)
            }),
            ..self
        }
    }

    pub fn reserved_width(self, mode: ReadingMode) -> f32 {
        if mode == ReadingMode::Classic && self.open && self.pinned {
            self.width
        } else {
            0.0
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredChrome {
    classic_sidebar: SidebarState,
}

#[derive(Default)]
pub(crate) struct SidebarPreferences {
    pub classic: SidebarState,
    path: Option<PathBuf>,
    pending: bool,
    writer: Option<tokio::sync::watch::Sender<SidebarState>>,
}

impl SidebarPreferences {
    pub fn load() -> Self {
        let path =
            crate::smoke::project_dirs().map(|dirs| dirs.config_dir().join("reader-ui.json"));
        let classic = path
            .as_ref()
            .and_then(|path| match std::fs::read(path) {
                Ok(bytes) => match serde_json::from_slice::<StoredChrome>(&bytes) {
                    Ok(stored) => Some(stored.classic_sidebar.constrained(f32::MAX)),
                    Err(error) => {
                        tracing::warn!(%error, "failed to load reader sidebar preferences");
                        None
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    tracing::warn!(%error, "failed to read reader sidebar preferences");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            classic,
            path,
            ..Default::default()
        }
    }

    pub fn remember(&mut self, state: SidebarState) {
        let state = state.constrained(f32::MAX);
        if self.classic != state {
            self.classic = state;
            self.pending = true;
        }
    }

    pub fn spawn_pending(&mut self, runtime: &tokio::runtime::Runtime) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        if let Some(writer) = &self.writer {
            writer.send_replace(self.classic);
            return;
        }
        // One latest-value slot and one serial writer: rapid changes cannot queue
        // unbounded writes or let an older write overwrite the latest preference.
        let (sender, mut receiver) = tokio::sync::watch::channel(self.classic);
        self.writer = Some(sender);
        runtime.spawn(async move {
            loop {
                let state = *receiver.borrow_and_update();
                let path = path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    crate::persistence::write_json_atomic(
                        &path,
                        &StoredChrome {
                            classic_sidebar: state,
                        },
                    )
                })
                .await;
                if !matches!(result, Ok(Ok(()))) {
                    tracing::warn!(?result, "failed to save reader sidebar preferences");
                }
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_title_uses_the_selected_anchor_or_its_chapter_and_never_another_document() {
        use rebook_publication::{PublicationUrl, TocEntry};
        let child = TocEntry {
            label: "Skin types".into(),
            href: Some(PublicationUrl::parse("beauty.xhtml#skin").unwrap()),
            children: vec![],
        };
        let entries = vec![TocEntry {
            label: "Skincare".into(),
            href: Some(PublicationUrl::parse("beauty.xhtml").unwrap()),
            children: vec![child],
        }];
        for (href, expected) in [
            ("beauty.xhtml#skin", Some("Skin types")),
            ("beauty.xhtml#paragraph-12", Some("Skincare")),
            ("beauty.xhtml", Some("Skincare")),
            ("another.xhtml", None),
        ] {
            assert_eq!(
                toc_label_for_href(&entries, &PublicationUrl::parse(href).unwrap()),
                expected
            );
        }
        assert_eq!(
            toc_label_for_href(
                &entries[0].children,
                &PublicationUrl::parse("beauty.xhtml").unwrap()
            ),
            None,
            "the first anchor cannot stand in for an unknown reading position",
        );
    }

    #[test]
    fn focus_visibility_is_temporary_and_classic_geometry_follows_saved_state() {
        let classic = SidebarState {
            open: false,
            pinned: true,
            width: 310.0,
        };
        assert_eq!(classic.for_mode(ReadingMode::Classic), classic);
        let focus = SidebarState::default().for_mode(ReadingMode::Focus);
        assert!(!focus.open && !focus.pinned);
        assert_eq!(classic.reserved_width(ReadingMode::Classic), 0.0);
        assert_eq!(
            SidebarState {
                open: true,
                ..classic
            }
            .reserved_width(ReadingMode::Classic),
            310.0
        );
        assert_eq!(
            SidebarState {
                open: true,
                ..focus
            }
            .reserved_width(ReadingMode::Focus),
            0.0
        );
        assert_eq!(SidebarState::default().constrained(350.0).width, 150.0);
        assert_eq!(
            serde_json::from_str::<StoredChrome>("{}")
                .unwrap()
                .classic_sidebar,
            SidebarState::default()
        );
    }

    #[test]
    fn background_writer_saves_latest_state_across_reopen() {
        let path = std::env::temp_dir().join(format!("reader-ui-{}.json", uuid::Uuid::new_v4()));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut preferences = SidebarPreferences {
            path: Some(path.clone()),
            ..Default::default()
        };
        for width in [260.0, 280.0, 320.0] {
            preferences.remember(SidebarState {
                open: false,
                pinned: false,
                width,
            });
            preferences.spawn_pending(&runtime);
        }
        let expected = preferences.classic;
        let sender = preferences.writer.take().unwrap();
        drop(sender);
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if std::fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<StoredChrome>(&bytes).ok())
                        .is_some_and(|stored| stored.classic_sidebar == expected)
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        });
        std::fs::remove_file(path).unwrap();
    }
}
