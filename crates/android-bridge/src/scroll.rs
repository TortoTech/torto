//! Bounded scroll composition. Retain at most eight nearby display lists, rasterize one viewport.
use crate::*;
use anyrender::PaintScene;
use kurbo::{Affine, Rect as KRect};
use peniko::{Color, Fill};
use rebook_reader::ReaderPosition;
use rebook_renderer::PageDisplayList;
#[derive(Clone)]
pub(crate) struct Slice {
    pub position: ReaderPosition,
    pub page: Arc<PageDisplayList>,
    pub top: f32,
    pub origin: f32,
    pub height: f32,
}
pub(crate) fn position(s: &ReaderState) -> ReaderPosition {
    let l = s.session.snapshot().location;
    ReaderPosition {
        section_index: l.section_index,
        segment_index: l.segment_index,
        page_index: l.page_index,
    }
}
fn extent(page: &PageDisplayList) -> (f32, f32) {
    let top = page.content_top().unwrap_or(0.0);
    (
        top,
        (page.content_bottom().unwrap_or(page.height() as f32) - top).max(1.0),
    )
}
fn step(s: &mut ReaderState, forward: bool) -> Result<bool> {
    let old = position(s);
    s.session
        .turn_page(if forward {
            PageDirection::Next
        } else {
            PageDirection::Previous
        })
        .map_err(error)?;
    Ok(position(s) != old)
}
fn center(s: &mut ReaderState) -> Result<()> {
    let mut units = interactions::focus_units(s)?;
    if let Some(anchor) = s.focus_anchor.clone() {
        // A paragraph can span logical pages. Keep the restored continuation
        // when it already belongs to the focused block instead of jumping to
        // the block's opening page after a font change.
        if !units.iter().any(|u| {
            u.ranges
                .iter()
                .any(|r| r.start.spine == anchor.start.spine && r.start.node == anchor.start.node)
        }) {
            s.session.go_to_source(&anchor.start).map_err(error)?;
            units = interactions::focus_units(s)?;
        }
    }
    let unit = s
        .focus_anchor
        .as_ref()
        .and_then(|a| {
            units.iter().find(|u| {
                u.ranges
                    .iter()
                    .any(|r| r.start.spine == a.start.spine && r.start.node == a.start.node)
            })
        })
        .or_else(|| units.first());
    if let Some(unit) = unit {
        let top = unit
            .bounds
            .iter()
            .map(|r| r.y)
            .fold(f32::INFINITY, f32::min);
        let bottom = unit
            .bounds
            .iter()
            .map(|r| r.y + r.height)
            .fold(0.0, f32::max);
        let height = s.session.viewport().height as f32;
        let origin = extent(s.session.current_page()).0;
        s.scroll_offset = if bottom - top > height * 0.8 {
            top - origin - 32.0
        } else {
            (top + bottom) / 2.0 - origin - height / 2.0
        };
        s.focus_anchor = unit.ranges.first().cloned();
    }
    s.scroll_center = false;
    Ok(())
}
fn window(s: &mut ReaderState) -> Result<Vec<Slice>> {
    let original = s.session.current_locator();
    let pos = position(s);
    let current = s.session.current_page_arc();
    let (origin, height) = extent(&current);
    let mut slices = vec![Slice {
        position: pos,
        page: current,
        top: -s.scroll_offset,
        origin,
        height,
    }];
    if s.scroll_offset < 0.0 && step(s, false)? {
        let page = s.session.current_page_arc();
        let (o, h) = extent(&page);
        let gap = slices[0].page.leading_gap();
        slices.insert(
            0,
            Slice {
                position: position(s),
                page,
                top: -s.scroll_offset - h - gap,
                origin: o,
                height: h + gap,
            },
        );
    }
    s.session.restore_locator(&original).map_err(error)?;
    let mut cursor = -s.scroll_offset + height;
    let mut overscan = 0;
    for _ in 0..6 {
        if cursor > s.session.viewport().height as f32 {
            overscan += 1;
            if overscan > 1 {
                break;
            }
        }
        if !step(s, true)? {
            break;
        }
        let page = s.session.current_page_arc();
        let (o, h) = extent(&page);
        let gap = page.leading_gap();
        if let Some(last) = slices.last_mut() {
            last.height += gap;
        }
        cursor += gap;
        slices.push(Slice {
            position: position(s),
            page,
            top: cursor,
            origin: o,
            height: h,
        });
        cursor += h;
    }
    s.session.restore_locator(&original).map_err(error)?;
    Ok(slices)
}
pub(crate) fn map_touch(s: &mut ReaderState, x: f32, y: f32) -> Result<(f32, f32)> {
    if !(s.focus && s.options.scroll_focus) {
        return Ok((x, y));
    }
    if let Some(slice) = s
        .scroll_slices
        .iter()
        .find(|p| y >= p.top && y <= p.top + p.height)
        .cloned()
    {
        let local = y - slice.top + slice.origin;
        if position(s) != slice.position {
            if s.session.set_visible_position(slice.position).is_err() {
                s.session
                    .go_to_section(slice.position.section_index)
                    .map_err(error)?;
                s.session
                    .set_visible_position(slice.position)
                    .map_err(error)?;
            }
            s.scroll_offset = -slice.top;
        }
        Ok((x, local))
    } else {
        Ok((x, y + s.scroll_offset + extent(s.session.current_page()).0))
    }
}
pub(crate) fn hit(
    s: &mut ReaderState,
    x: f32,
    y: f32,
    nearest: bool,
) -> Result<Option<rebook_reader::ReaderTextHit>> {
    if s.focus && s.options.scroll_focus {
        if let Some(slice) = s
            .scroll_slices
            .iter()
            .find(|p| y >= p.top && y <= p.top + p.height)
            .cloned()
        {
            return s
                .session
                .hit_test_page(slice.position, x, y - slice.top + slice.origin, nearest)
                .map_err(error);
        }
        Ok(None)
    } else {
        s.session
            .hit_test_current_spread(x, y, nearest)
            .map_err(error)
    }
}
pub(crate) fn selected(s: &ReaderState) -> Option<SelectedText> {
    let selected = s.selected.as_ref()?;
    let bounds = if s.focus && s.options.scroll_focus {
        s.scroll_slices
            .iter()
            .flat_map(|slice| {
                slice
                    .page
                    .source_rects(&selected.ranges)
                    .into_iter()
                    .map(move |r| Rect {
                        x: r.x0 as f32,
                        y: r.y0 as f32 + slice.top - slice.origin,
                        width: r.width() as f32,
                        height: r.height() as f32,
                    })
            })
            .collect()
    } else {
        selected
            .rects
            .iter()
            .map(|r| Rect {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
            })
            .collect()
    };
    Some(SelectedText {
        text: selected.text.clone(),
        bounds,
    })
}
pub(crate) fn visible_ranges(s: &ReaderState) -> Vec<SourceRange> {
    if !(s.focus && s.options.scroll_focus) {
        return Vec::new();
    }
    let h = s.session.viewport().height as f32;
    s.scroll_slices
        .iter()
        .flat_map(|p| {
            p.page.content_sources().into_iter().filter(move |r| {
                p.page
                    .source_block_bounds(std::slice::from_ref(r))
                    .iter()
                    .any(|b| {
                        b.y1 as f32 + p.top - p.origin > 0.0 && b.y0 as f32 + p.top - p.origin < h
                    })
            })
        })
        .collect()
}
pub(crate) fn render(s: &mut ReaderState) -> Result<Arc<MobilePage>> {
    if s.scroll_center {
        center(s)?;
    }
    let anchor = s.focus_anchor.clone();
    let mut info = page_info(s)?;
    s.focus_anchor = anchor;
    let slices = window(s)?;
    let viewport = s.session.viewport();
    let w = viewport.width;
    let h = viewport.height;
    let mut units = Vec::<interactions::FocusUnit>::new();
    let mut regions = Vec::new();
    let mut marks = Vec::new();
    let saved = s.store.annotations_for_book(&s.book_id).map_err(error)?;
    for p in &slices {
        let section = s
            .source
            .parse_section(p.position.section_index)
            .map_err(error)?;
        for mut unit in interactions::focus_units_on(&section, &p.page) {
            for b in &mut unit.bounds {
                b.y += p.top - p.origin;
            }
            if let Some(old) = units.iter_mut().find(|u| u.ranges == unit.ranges) {
                old.bounds.extend(unit.bounds);
            } else {
                units.push(unit);
            }
        }
        for fragment in s
            .session
            .cached_visible_text_fragments_for_pages(&[p.position])
        {
            let rects = p.page.source_rects(std::slice::from_ref(&fragment.range));
            if rects.is_empty() {
                continue;
            }
            regions.push(TextRegion {
                text: fragment.text,
                bounds: rects
                    .into_iter()
                    .map(|r| Rect {
                        x: r.x0 as f32,
                        y: r.y0 as f32 + p.top - p.origin,
                        width: r.width() as f32,
                        height: r.height() as f32,
                    })
                    .collect(),
                source: serde_json::to_string(&fragment.range).map_err(error)?,
            });
        }
        for annotation in &saved {
            marks.extend(
                p.page
                    .source_rects(&annotation.ranges)
                    .into_iter()
                    .map(|r| Rect {
                        x: r.x0 as f32,
                        y: r.y0 as f32 + p.top - p.origin,
                        width: r.width() as f32,
                        height: r.height() as f32,
                    }),
            );
        }
    }
    let index = s
        .focus_anchor
        .as_ref()
        .and_then(|a| {
            units.iter().position(|u| {
                u.ranges
                    .iter()
                    .any(|r| r.start.spine == a.start.spine && r.start.node == a.start.node)
            })
        })
        .unwrap_or_else(|| {
            units
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let distance = |u: &interactions::FocusUnit| {
                        u.bounds
                            .iter()
                            .map(|r| ((r.y + r.height / 2.0) - h as f32 / 2.0).abs())
                            .fold(f32::INFINITY, f32::min)
                    };
                    distance(a).total_cmp(&distance(b))
                })
                .map_or(0, |(i, _)| i)
        });
    info.focus_bounds = units.get(index).map_or_else(Vec::new, |u| u.bounds.clone());
    info.focus_index = index as u32;
    info.focus_count = units.len() as u32;
    info.regions = regions;
    info.marks = marks;
    info.is_scroll = true;
    info.scroll_distance = s.scroll_distance;
    s.focus_anchor = units.get(index).and_then(|u| u.ranges.first().cloned());
    s.focus_units = units;
    let scale = viewport.raster_scale;
    let mut rasterizer = anyrender_vello_cpu::VelloCpuImageRenderer::new(
        (w as f32 * scale).ceil() as u32,
        (h as f32 * scale).ceil() as u32,
    );
    let mut pixels = Vec::new();
    rasterizer.render_to_vec(
        |scene| {
            let color = s.session.style().background;
            scene.fill(
                Fill::NonZero,
                Affine::scale(f64::from(scale)),
                Color::from_rgba8(color.red, color.green, color.blue, 255),
                None,
                &KRect::new(0.0, 0.0, f64::from(w), f64::from(h)),
            );
            let ink = s.session.style().foreground;
            for (index, p) in slices.iter().enumerate() {
                if p.top > h as f32 || p.top + p.height < 0.0 {
                    continue;
                }
                if let Some(next) = slices.get(index + 1) {
                    if let Some(bridge) = p.page.quote_bridge_to(&next.page) {
                        let bottom = p.top + extent(&p.page).1;
                        scene.fill(
                            Fill::NonZero,
                            Affine::scale(f64::from(scale)),
                            bridge.color,
                            None,
                            &KRect::new(
                                f64::from(bridge.x),
                                f64::from(bottom),
                                f64::from(bridge.x + bridge.width),
                                f64::from(next.top),
                            ),
                        );
                    }
                }
                scene.push_clip_layer(
                    Fill::NonZero,
                    Affine::scale(f64::from(scale)),
                    &KRect::new(
                        0.0,
                        f64::from(p.top),
                        f64::from(w),
                        f64::from(p.top + p.height),
                    ),
                );
                p.page.paint_scaled_at(scene, scale, 0.0, p.top - p.origin);
                p.page.paint_footnote_icons_transformed(
                    scene,
                    Color::from_rgba8(ink.red, ink.green, ink.blue, 255),
                    Affine::scale(f64::from(scale))
                        * Affine::translate((0.0, f64::from(p.top - p.origin))),
                );
                scene.pop_layer();
            }
        },
        &mut pixels,
    );
    s.scroll_slices = slices;
    Ok(MobilePage::new(
        (w as f32 * scale).ceil() as u32,
        (h as f32 * scale).ceil() as u32,
        pixels,
        info,
    ))
}
pub(crate) fn turn(s: &mut ReaderState, forward: bool) -> Result<Arc<MobilePage>> {
    let index = s
        .focus_anchor
        .as_ref()
        .and_then(|a| {
            s.focus_units
                .iter()
                .position(|u| u.ranges.iter().any(|r| r.start == a.start))
        })
        .unwrap_or(0);
    if let Some(unit) = s.focus_units.get(index) {
        let top = unit
            .bounds
            .iter()
            .map(|r| r.y)
            .fold(f32::INFINITY, f32::min);
        let bottom = unit
            .bounds
            .iter()
            .map(|r| r.y + r.height)
            .fold(0.0, f32::max);
        let height = s.session.viewport().height as f32;
        if (forward && bottom > height - 16.0) || (!forward && top < 16.0) {
            return scroll(s, if forward { height * 0.7 } else { -height * 0.7 }, false);
        }
    }
    let next = if forward {
        index.checked_add(1).filter(|i| *i < s.focus_units.len())
    } else {
        index.checked_sub(1)
    };
    if let Some(next) = next {
        s.focus_anchor = s.focus_units[next].ranges.first().cloned();
        s.scroll_center = true;
    } else {
        step(s, forward)?;
        s.focus_anchor = None;
        s.scroll_center = true;
    }
    render(s)
}
fn scroll(s: &mut ReaderState, delta: f32, follow_center: bool) -> Result<Arc<MobilePage>> {
    if !delta.is_finite() {
        return Err(error("滚动距离无效"));
    }
    s.scroll_offset += delta.clamp(
        -4.0 * s.session.viewport().height as f32,
        4.0 * s.session.viewport().height as f32,
    );
    s.scroll_distance += f64::from(delta);
    for _ in 0..8 {
        let (_, height) = extent(s.session.current_page());
        if s.scroll_offset >= height {
            if !step(s, true)? {
                s.scroll_offset = s.scroll_offset.min(height);
                break;
            }
            s.scroll_offset -= height + s.session.current_page().leading_gap();
        } else if s.scroll_offset < -(s.session.viewport().height as f32) / 2.0 {
            if !step(s, false)? {
                s.scroll_offset = s
                    .scroll_offset
                    .max(-(s.session.viewport().height as f32) / 2.0);
                break;
            }
            s.scroll_offset += extent(s.session.current_page()).1;
        } else {
            break;
        }
    }
    if follow_center {
        s.focus_anchor = None;
    }
    render(s)
}
#[uniffi::export]
impl MobileReader {
    pub async fn scroll_by(self: Arc<Self>, delta: f32) -> Result<Arc<MobilePage>> {
        work(move || self.with(|s| scroll(s, delta, true))).await
    }
    pub async fn selection(self: Arc<Self>) -> Result<Option<SelectedText>> {
        work(move || self.with(|s| Ok(selected(s)))).await
    }
    pub async fn begin_selection_handle(self: Arc<Self>, start: bool) -> Result<()> {
        work(move || {
            self.with(|s| {
                s.selection_drag_anchor = if start {
                    s.selection_end.clone()
                } else {
                    s.selection_start.clone()
                };
                Ok(())
            })
        })
        .await
    }
    pub async fn extend_selection(self: Arc<Self>, x: f32, y: f32) -> Result<Option<SelectedText>> {
        work(move || {
            self.with(|s| {
                if !x.is_finite() || !y.is_finite() {
                    return Err(error("选择坐标无效"));
                }
                let Some(anchor) = s.selection_drag_anchor.clone() else {
                    return Ok(None);
                };
                let Some(focus) = hit(s, x, y, true)? else {
                    return Ok(selected(s));
                };
                s.selected = s
                    .session
                    .selection_between_with_granularity(&anchor, &focus, s.selection_mode)
                    .map_err(error)?;
                if anchor.ordering_key() <= focus.ordering_key() {
                    s.selection_start = Some(anchor);
                    s.selection_end = Some(focus);
                } else {
                    s.selection_start = Some(focus);
                    s.selection_end = Some(anchor);
                }
                Ok(selected(s))
            })
        })
        .await
    }
}
