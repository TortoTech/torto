//! Keep the current reading source at its screen coordinate across content updates.
use crate::*;
use rebook_app_core::reading_text::source_range_contains_anchor;

#[cfg(test)]
pub(crate) mod tests;

struct Anchor {
    focus: SourceRange,
    source: SourceRange,
    screen_y: f32,
    baseline: bool,
}

impl Anchor {
    fn capture(s: &ReaderState) -> Option<Self> {
        if !s.focus {
            return None;
        }
        let focus = s.focus_anchor.as_ref()?;
        let unit = s.focus_units.iter().find(|unit| {
            unit.ranges
                .iter()
                .any(|r| source_range_contains_anchor(r, &focus.start))
        })?;
        let height = s.session.viewport().height as f32;
        let mut lines = s
            .scroll_slices
            .iter()
            .flat_map(|slice| {
                slice
                    .page
                    .source_rects(&unit.ranges)
                    .into_iter()
                    .filter_map(move |rect| {
                        let y = rect.y0 as f32 + slice.top - slice.origin;
                        (y + rect.height() as f32 > 0.0 && y < height).then_some((slice, rect, y))
                    })
            })
            .collect::<Vec<_>>();
        lines.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.1.x0.total_cmp(&b.1.x0)));
        for (slice, rect, _) in lines {
            let page = &slice.page;
            let Some(hit) = page.hit_test_text(rect.x0 as f32 + 0.5, rect.center().y as f32, false)
            else {
                continue;
            };
            let bytes = if hit.cluster_start < hit.cluster_end {
                hit.cluster_start..hit.cluster_end
            } else {
                let text = page.text_region_text(hit.region_index)?;
                let visible = page.text_region_visible_range(hit.region_index)?;
                let start = hit.byte_index.clamp(visible.start, visible.end);
                let Some(ch) = text.get(start..visible.end).and_then(|t| t.chars().next()) else {
                    continue;
                };
                start..start + ch.len_utf8()
            };
            let Some(fragment) = page.selection_fragment(hit.region_index, bytes) else {
                continue;
            };
            if !unit
                .ranges
                .iter()
                .any(|r| source_range_contains_anchor(r, &fragment.range.start))
            {
                continue;
            }
            let baseline = page.source_text_baseline(&fragment.range);
            return Some(Self {
                focus: focus.clone(),
                source: fragment.range,
                screen_y: baseline.unwrap_or(rect.y0 as f32) + slice.top - slice.origin,
                baseline: baseline.is_some(),
            });
        }
        // Images, formulas and charts can have no selectable text.
        Some(Self {
            focus: focus.clone(),
            source: focus.clone(),
            screen_y: unit.navigation_bounds()?.y,
            baseline: false,
        })
    }

    fn restore(&self, s: &mut ReaderState) -> bool {
        if s.session.go_to_source(&self.source.start).is_err() {
            return false;
        }
        let page = s.session.current_page();
        let y = self
            .baseline
            .then(|| page.source_text_baseline(&self.source))
            .flatten()
            .or_else(|| {
                page.source_content_bounds(std::slice::from_ref(&self.source))
                    .map(|r| r.y0 as f32)
            });
        let Some(y) = y else { return false };
        let origin = scroll::current_extent(s).0;
        s.scroll_offset = y - origin - self.screen_y;
        s.scroll_center = false;
        s.focus_anchor = Some(self.focus.clone());
        true
    }
}

/// Capture from the last displayed geometry, never the viewport that sent HTTP.
/// Source overlays may already contain new data; retained pages still describe
/// exactly what the reader was seeing before this refresh.
pub(crate) fn refresh(s: &mut ReaderState) -> Result<()> {
    let anchor = Anchor::capture(s);
    let mut style = s.session.style();
    style.typesetting =
        rebook_app_core::reading::effective_typesetting(s.focus, &s.classic_typesetting);
    s.session.refresh_source_with_style(style).map_err(error)?;
    scroll::invalidate(s);
    if let Some(anchor) = anchor {
        let fallback = s.session.current_locator();
        if !anchor.restore(s) {
            s.session.restore_locator(&fallback).map_err(error)?;
            s.focus_anchor = None;
            s.scroll_center = true;
        }
    } else if s.focus {
        s.scroll_center = true;
    }
    s.enhancements.dirty = false;
    Ok(())
}
