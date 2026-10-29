//! Search cards retain the matched occurrence when fitting a preview to the sidebar.
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use egui::{Color32, FontId, Galley, Rect, Vec2};

use crate::plugins::BookSearchResult;
use crate::preferences::AppLanguage;
use crate::ui::{palette, scaled_font_size};

const GAP: f32 = 8.0;
const PADDING: Vec2 = Vec2::new(10.0, 8.0);
const TEXT_GAP: f32 = 4.0;

#[derive(Default)]
pub(super) struct SearchResults {
    key: Option<LayoutKey>,
    cards: Vec<Card>,
    height: f32,
}

#[derive(PartialEq)]
struct LayoutKey {
    width: f32,
    pixels_per_point: f32,
    heading_font: FontId,
    body_font: FontId,
    fonts: u64,
    line_spacing: f32,
    colors: [Color32; 3],
    language: AppLanguage,
}

struct Card {
    top: f32,
    height: f32,
    heading: Arc<Galley>,
    preview: Arc<Galley>,
}

impl SearchResults {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        results: &[BookSearchResult],
        language: AppLanguage,
    ) -> Option<usize> {
        egui::ScrollArea::vertical()
            .id_salt("book-search-results")
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let scroll = &ui.spacing().scroll;
                let gutter = if scroll.floating {
                    (scroll.bar_width + scroll.bar_inner_margin + scroll.bar_outer_margin + 4.0)
                        .max(14.0)
                } else {
                    6.0
                };
                let width = (ui.available_width() - gutter).max(1.0);
                self.prepare(ui, results, language, width);
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(width, self.height), egui::Sense::hover());
                let first = self
                    .cards
                    .partition_point(|card| card.top + card.height < viewport.top());
                let mut clicked = None;
                for (index, card) in self.cards.iter().enumerate().skip(first) {
                    if card.top > viewport.bottom() {
                        break;
                    }
                    let card_rect = Rect::from_min_size(
                        rect.min + Vec2::new(0.0, card.top),
                        Vec2::new(width, card.height),
                    );
                    if !ui.is_rect_visible(card_rect) {
                        continue;
                    }
                    let response = ui
                        .interact(card_rect, ui.scope_id().with(index), egui::Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    let colors = palette();
                    let fill = if response.hovered() || response.is_pointer_button_down_on() {
                        colors.surface_muted
                    } else {
                        colors.surface
                    };
                    let border = if response.hovered() {
                        colors.accent.gamma_multiply(0.4)
                    } else {
                        colors.border
                    };
                    let painter = ui
                        .painter()
                        .with_clip_rect(ui.clip_rect().intersect(card_rect));
                    painter.rect(
                        card_rect,
                        7.0,
                        fill,
                        egui::Stroke::new(1.0, border),
                        egui::StrokeKind::Inside,
                    );
                    let text_top = card_rect.min + PADDING;
                    painter.galley(text_top, Arc::clone(&card.heading), colors.muted);
                    painter.galley(
                        text_top + Vec2::new(0.0, card.heading.size().y + TEXT_GAP),
                        Arc::clone(&card.preview),
                        colors.text,
                    );
                    if response.clicked() {
                        clicked = Some(index);
                    }
                }
                clicked
            })
            .inner
    }

    fn prepare(
        &mut self,
        ui: &egui::Ui,
        results: &[BookSearchResult],
        language: AppLanguage,
        width: f32,
    ) {
        let colors = palette();
        let fonts = ui.fonts(|fonts| {
            let definitions = fonts.definitions();
            let mut hash = DefaultHasher::new();
            definitions.families.hash(&mut hash);
            // Hash identities, not font bytes. Fallback changes invalidate cached galleys.
            for (name, data) in &definitions.font_data {
                name.hash(&mut hash);
                Arc::as_ptr(data).hash(&mut hash);
            }
            hash.finish()
        });
        let matched = super::SEARCH_MARK_COLOR.to_rgba8();
        let key = LayoutKey {
            width,
            pixels_per_point: ui.ctx().pixels_per_point(),
            heading_font: FontId::proportional(scaled_font_size(11.0)),
            body_font: FontId::proportional(scaled_font_size(12.0)),
            fonts,
            line_spacing: ui.spacing().extra_text_line_spacing,
            colors: [
                colors.muted,
                colors.text,
                Color32::from_rgba_unmultiplied(matched.r, matched.g, matched.b, matched.a),
            ],
            language,
        };
        if self.key.as_ref() == Some(&key) && self.cards.len() == results.len() {
            return;
        }
        let text_width = (width - PADDING.x * 2.0).max(1.0);
        let rows = if text_width < key.body_font.size * 18.0 {
            3
        } else {
            2
        };
        let mut top = 0.0;
        self.cards = ui.fonts_mut(|fonts| {
            results
                .iter()
                .map(|result| {
                    let title = result.section_title.trim();
                    let title = if title.is_empty() {
                        language.text("正文", "Text")
                    } else {
                        title
                    };
                    let heading = layout_text(
                        fonts,
                        title,
                        0..0,
                        &key.heading_font,
                        key.colors[0],
                        Color32::TRANSPARENT,
                        text_width,
                        key.line_spacing,
                        1,
                    );
                    let preview = fit_preview(fonts, result, &key, text_width, rows);
                    let height = PADDING.y * 2.0 + heading.size().y + TEXT_GAP + preview.size().y;
                    let card = Card {
                        top,
                        height,
                        heading,
                        preview,
                    };
                    top += height + GAP;
                    card
                })
                .collect()
        });
        self.height = (top - GAP).max(0.0);
        self.key = Some(key);
    }
}

fn layout_text(
    fonts: &mut egui::epaint::text::FontsView<'_>,
    text: &str,
    matched: Range<usize>,
    font: &FontId,
    color: Color32,
    highlight: Color32,
    width: f32,
    line_spacing: f32,
    rows: usize,
) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = rows;
    let format = egui::TextFormat {
        font_id: font.clone(),
        color,
        line_height: Some(fonts.row_height(font) + line_spacing),
        ..Default::default()
    };
    let append = |job: &mut egui::text::LayoutJob, text: &str, background| {
        let mut format = format.clone();
        format.background = background;
        job.append(text, 0.0, format);
    };
    if let (Some(before), Some(found), Some(after)) = (
        text.get(..matched.start),
        text.get(matched.clone()),
        text.get(matched.end..),
    ) {
        append(&mut job, before, Color32::TRANSPARENT);
        append(&mut job, found, highlight);
        append(&mut job, after, Color32::TRANSPARENT);
    } else {
        append(&mut job, text, Color32::TRANSPARENT);
    }
    fonts.layout_job(job)
}

fn fit_preview(
    fonts: &mut egui::epaint::text::FontsView<'_>,
    result: &BookSearchResult,
    key: &LayoutKey,
    width: f32,
    rows: usize,
) -> Arc<Galley> {
    let layout = |fonts: &mut egui::epaint::text::FontsView<'_>,
                  text: &str,
                  matched: Range<usize>,
                  max_rows| {
        layout_text(
            fonts,
            text,
            matched,
            &key.body_font,
            key.colors[1],
            key.colors[2],
            width,
            key.line_spacing,
            max_rows,
        )
    };
    let full = layout(
        fonts,
        &result.excerpt,
        result.excerpt_match.clone(),
        usize::MAX,
    );
    if full.rows.len() <= rows {
        return full;
    }
    let boundaries: Vec<_> = result
        .excerpt
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(result.excerpt.len()))
        .collect();
    let Ok(first) = boundaries.binary_search(&result.excerpt_match.start) else {
        return full;
    };
    let Ok(last) = boundaries.binary_search(&result.excerpt_match.end) else {
        return full;
    };
    let before = first;
    let after = boundaries.len() - 1 - last;
    let candidate = |budget: usize| {
        let mut left = (budget / 2).min(before);
        let right = (budget - left).min(after);
        left = (budget - right).min(before);
        let start = boundaries[first - left];
        let end = boundaries[last + right];
        let prefix = if start > 0 { "…" } else { "" };
        let text = format!(
            "{prefix}{}{}",
            &result.excerpt[start..end],
            if end < result.excerpt.len() {
                "…"
            } else {
                ""
            }
        );
        let match_start = prefix.len() + result.excerpt_match.start - start;
        (text, match_start..match_start + result.excerpt_match.len())
    };
    let (text, matched) = candidate(0);
    // Extremely long queries can themselves exceed the preview. Show their
    // highlighted beginning instead of letting preceding context hide them.
    let mut best = layout(fonts, &text, matched, rows);
    let (mut low, mut high) = (0, before + after);
    while low < high {
        let budget = (low + high).div_ceil(2);
        let (text, matched) = candidate(budget);
        let galley = layout(fonts, &text, matched, usize::MAX);
        if galley.rows.len() <= rows {
            low = budget;
            best = galley;
        } else {
            high = budget - 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{SourceAnchor, SourceRange, SpineItemId};

    fn result(text: &str, matched: Range<usize>) -> BookSearchResult {
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        BookSearchResult {
            section_index: 0,
            section_title: "Chapter 1".into(),
            excerpt: text.into(),
            matched_text: text[matched.clone()].into(),
            excerpt_match: matched,
            block_kind: "paragraph".into(),
            range: SourceRange {
                start: anchor.clone(),
                end: anchor,
            },
        }
    }

    fn highlighted(galley: &Galley) -> String {
        galley
            .job
            .sections
            .iter()
            .filter(|section| section.format.background != Color32::TRANSPARENT)
            .map(|section| &galley.job.text[section.byte_range.start.0..section.byte_range.end.0])
            .collect()
    }

    fn key(width: f32, size: f32) -> LayoutKey {
        LayoutKey {
            width,
            pixels_per_point: 1.0,
            heading_font: FontId::proportional(size - 1.0),
            body_font: FontId::proportional(size),
            fonts: 0,
            line_spacing: 1.0,
            colors: [Color32::GRAY, Color32::BLACK, Color32::YELLOW],
            language: AppLanguage::English,
        }
    }

    #[test]
    fn fitting_long_previews_keeps_the_exact_match_at_large_fonts_and_narrow_widths() {
        let ctx = egui::Context::default();
        let text = format!(
            "{}纤维{}",
            "very long unrelated context ".repeat(5),
            " more context".repeat(6)
        );
        let at = text.find("纤维").unwrap();
        let result = result(&text, at..at + "纤维".len());
        let output = ctx.run_ui(egui::RawInput::default(), |_| {
            ctx.fonts_mut(|fonts| {
                for width in [100.0, 180.0, 300.0] {
                    for size in [12.0, 24.0] {
                        let preview = fit_preview(fonts, &result, &key(width, size), width, 3);
                        assert!(preview.rows.len() <= 3);
                        assert!(!preview.elided);
                        assert_eq!(highlighted(&preview), "纤维");
                        assert!(preview.job.text.contains("纤维"));
                    }
                }
            });
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn repeated_case_insensitive_matches_highlight_only_the_selected_occurrence() {
        let ctx = egui::Context::default();
        let text = "Systems thinking helps us see systems.";
        let at = text.rfind("systems").unwrap();
        let result = result(text, at..at + 7);
        let output = ctx.run_ui(egui::RawInput::default(), |_| {
            ctx.fonts_mut(|fonts| {
                let preview = fit_preview(fonts, &result, &key(300.0, 12.0), 300.0, 2);
                assert_eq!(highlighted(&preview), "systems");
                let found = preview
                    .job
                    .sections
                    .iter()
                    .find(|section| section.format.background == Color32::YELLOW)
                    .unwrap();
                assert_eq!(found.byte_range.start.0, at);
            });
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn cards_keep_their_full_height_gap_and_scrollbar_gutter_and_only_paint_visible_rows() {
        let ctx = egui::Context::default();
        let mut cache = SearchResults::default();
        let results = vec![result("A sentence with keyword and surrounding context.", 16..23); 200];
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(400.0, 220.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input.clone(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.spacing_mut().scroll.floating = true;
                ui.spacing_mut().scroll.bar_width = 8.0;
                cache.show(ui, &results, AppLanguage::English);
            });
        });
        output.textures_delta.clear();
        assert_eq!(cache.cards.len(), 200);
        let width = cache.key.as_ref().unwrap().width;
        let cards: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if (rect.rect.width() - width).abs() < 0.1
                        && rect.corner_radius == egui::CornerRadius::same(7) =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .collect();
        assert!(
            cards.len() >= 2 && cards.len() < 10,
            "painted {} cards",
            cards.len()
        );
        for pair in cards.windows(2) {
            assert!(
                (pair[1].top() - pair[0].bottom() - GAP).abs() < 0.1,
                "expected {GAP} gap between {pair:?}; all rectangles: {cards:?}"
            );
        }
        assert!(cards[0].right() <= 400.0 - 8.0 - 14.0);
        for card in &cache.cards {
            assert!(
                card.height
                    >= card.heading.size().y + card.preview.size().y + PADDING.y * 2.0 + TEXT_GAP
            );
        }
        let preview = Arc::clone(&cache.cards[0].preview);
        let output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.spacing_mut().scroll.floating = true;
                ui.spacing_mut().scroll.bar_width = 8.0;
                cache.show(ui, &results, AppLanguage::English);
            });
        });
        output.drop_without_applying_deltas();
        assert!(
            Arc::ptr_eq(&preview, &cache.cards[0].preview),
            "settled frames reuse cached layout"
        );
    }

    #[test]
    fn clicking_after_scrolling_returns_the_original_result_index() {
        let ctx = egui::Context::default();
        let mut cache = SearchResults::default();
        let results = vec![result("keyword appears here", 0..7); 200];
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(400.0, 220.0),
            )),
            focused: true,
            time: Some(0.0),
            ..Default::default()
        };
        let mut scroll_id = egui::Id::NULL;
        let output = ctx.run_ui(input.clone(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                scroll_id = ui.make_persistent_id(egui::IdSalt::new("book-search-results"));
                cache.show(ui, &results, AppLanguage::English);
            });
        });
        output.drop_without_applying_deltas();
        let mut state = egui::scroll_area::State::load(&ctx, scroll_id).unwrap();
        state.set_offset(egui::vec2(state.unclamped_offset().x, cache.cards[100].top));
        state.store(&ctx, scroll_id);
        // Publish the scrolled widget positions before hit testing the next click.
        let output = ctx.run_ui(input.clone(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                cache.show(ui, &results, AppLanguage::English);
            });
        });
        output.drop_without_applying_deltas();
        let position = egui::Pos2::new(30.0, 25.0);
        let mut selected = None;
        for (time, pressed) in [(0.1, true), (0.2, false)] {
            let mut input = input.clone();
            input.time = Some(time);
            input.events = vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ];
            let output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    selected = cache.show(ui, &results, AppLanguage::English).or(selected);
                });
            });
            output.drop_without_applying_deltas();
        }
        assert_eq!(selected, Some(100));
    }

    #[test]
    fn font_changes_and_new_results_invalidate_card_layouts() {
        let ctx = egui::Context::default();
        let mut cache = SearchResults::default();
        let results = vec![result("keyword appears here", 0..7)];
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                cache.prepare(ui, &results, AppLanguage::English, 240.0)
            });
        });
        output.drop_without_applying_deltas();
        let old = Arc::clone(&cache.cards[0].preview);
        let mut definitions = egui::FontDefinitions::default();
        let font = Arc::clone(definitions.font_data.values().next().unwrap());
        definitions.font_data.insert("extra-font".into(), font);
        ctx.set_fonts(definitions);
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                cache.prepare(ui, &results, AppLanguage::English, 240.0)
            });
        });
        output.drop_without_applying_deltas();
        assert!(!Arc::ptr_eq(&old, &cache.cards[0].preview));
        cache.clear();
        assert!(cache.cards.is_empty() && cache.key.is_none());
    }
}
