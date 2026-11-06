//! Bounded retained scroll strips; gesture frames translate pixels and overlays independently.
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
/// Durable locators may point to the beginning of a paragraph spanning pages.
/// Temporary peeks must restore the exact logical page as well as the source.
pub(crate) fn restore_position(s: &mut ReaderState, locator: &rebook_publication::LocatorV1, position: ReaderPosition) -> Result<()> {
    s.session.restore_locator(locator).map_err(error)?;
    s.session.set_visible_position(position).map_err(error)?;
    Ok(())
}
fn extent(page: &PageDisplayList) -> (f32, f32) {
    let top = page.content_top().unwrap_or(0.0);
    (
        top,
        (page.content_bottom().unwrap_or(page.height() as f32) - top).max(1.0),
    )
}
pub(crate) fn current_extent(s: &ReaderState) -> (f32, f32) {
    let position = position(s);
    let page = s.session.current_page();
    // Use the same semantic crops as desktop, including units sharing a spine.
    let entry = s.session.cached_reading_unit_pages().into_iter().find(|p| p.position == position);
    let top = entry.as_ref().and_then(|p| p.visible_top).or_else(|| page.content_top()).unwrap_or(0.0);
    let bottom = entry.as_ref().and_then(|p| p.visible_bottom).or_else(|| page.content_bottom()).unwrap_or(page.height() as f32);
    (top, (bottom-top).max(1.0))
}
fn unit_key(s: &mut ReaderState) -> (std::ops::Range<usize>, usize) {
    (s.session.current_reading_unit_sections(), s.session.reading_unit_location().index)
}
fn step(s: &mut ReaderState, forward: bool) -> Result<bool> {
    let original = s.session.current_locator();
    let old = position(s);
    let unit = unit_key(s);
    s.session
        .turn_page(if forward {
            PageDirection::Next
        } else {
            PageDirection::Previous
        })
        .map_err(error)?;
    // Raw logical turns leave the semantic cursor untouched. Synchronize it
    // before comparing units, especially two TOC fragments in the same spine.
    s.session.set_visible_position(position(s)).map_err(error)?;
    if unit_key(s) != unit {
        restore_position(s, &original, old)?;
        return Ok(false);
    }
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
        let rect = unit.navigation_bounds().expect("focus body geometry");
        let top = rect.y;
        let bottom = rect.y + rect.height;
        let height = s.session.viewport().height as f32;
        let origin = current_extent(s).0;
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
fn window(s: &mut ReaderState, lower: f32, upper: f32) -> Result<Vec<Slice>> {
    let original = s.session.current_locator();
    let original_position = position(s);
    let current = s.session.current_page_arc();
    let (origin, height) = current_extent(s);
    let mut slices = vec![Slice {
        position: position(s),
        page: current,
        top: -s.scroll_offset,
        origin,
        height,
    }];
    for _ in 0..3 {
        if slices[0].top <= lower {
            break;
        }
        if !step(s, false)? {
            break;
        }
        let page = s.session.current_page_arc();
        let (origin, height) = current_extent(s);
        let gap = slices[0].page.leading_gap();
        let top = slices[0].top - height - gap;
        slices.insert(
            0,
            Slice {
                position: position(s),
                page,
                top,
                origin,
                height: height + gap,
            },
        );
    }
    restore_position(s, &original, original_position)?;
    let mut cursor = -s.scroll_offset + height;
    let mut extra = false;
    while slices.len() < 8 {
        if cursor >= upper {
            if extra {
                break;
            }
            extra = true;
        }
        if !step(s, true)? {
            break;
        }
        let page = s.session.current_page_arc();
        let (origin, height) = current_extent(s);
        let gap = page.leading_gap();
        if let Some(last) = slices.last_mut() {
            last.height += gap;
        }
        cursor += gap;
        slices.push(Slice {
            position: position(s),
            page,
            top: cursor,
            origin,
            height,
        });
        cursor += height;
    }
    restore_position(s, &original, original_position)?;
    Ok(slices)
}
pub(crate) fn map_touch(s: &mut ReaderState, x: f32, y: f32) -> Result<(f32, f32)> {
    if !s.focus {
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
        Ok((x, y + s.scroll_offset + current_extent(s).0))
    }
}
pub(crate) fn hit(
    s: &mut ReaderState,
    x: f32,
    y: f32,
    nearest: bool,
) -> Result<Option<rebook_reader::ReaderTextHit>> {
    if s.focus {
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
    let bounds = if s.focus {
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
    if !s.focus {
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
// One retained strip per reader. Native pixels and the Android bitmap each use
// at most 24 MiB (or the existing single-viewport allowance for huge screens).
const RASTER_BUDGET: usize = 24 * 1024 * 1024;
pub(crate) struct Cache {
    raster: Arc<MobilePage>,
    distance: f64,
    top: f32,
    height: f32,
    slices: Vec<Slice>,
    units: Vec<interactions::FocusUnit>,
    regions: Vec<TextRegion>,
}
pub(crate) fn context_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn invalidate(s: &mut ReaderState) {
    s.scroll_cache = None;
    s.scroll_slices.clear();
    s.scroll_distance = 0.0;
    s.scroll_context = context_id();
}
fn shift_bounds(bounds: &mut [Rect], delta: f32) {
    for r in bounds {
        r.y -= delta;
    }
}
fn bounds(unit: &interactions::FocusUnit) -> (f32, f32) {
    let rect=unit.navigation_bounds().expect("focus body geometry");
    (rect.y,rect.y+rect.height)
}
fn active(s: &ReaderState) -> usize {
    s.focus_anchor
        .as_ref()
        .and_then(|a| {
            s.focus_units.iter().position(|u| {
                u.ranges
                    .iter()
                    .any(|r| r.start.spine == a.start.spine && r.start.node == a.start.node)
            })
        })
        .unwrap_or_else(|| {
            s.focus_units
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let distance = |u: &interactions::FocusUnit| {
                        let (a, b) = bounds(u);
                        ((a + b) / 2.0 - s.session.viewport().height as f32 / 2.0).abs()
                    };
                    distance(a).total_cmp(&distance(b))
                })
                .map_or(0, |(i, _)| i)
        })
}
fn destination(s: &ReaderState, index: usize, forward: bool) -> Option<(usize, FocusTarget)> {
    let height = s.session.viewport().height as f32;
    let (top, bottom) = bounds(s.focus_units.get(index)?);
    if bottom - top > height - 64.0 {
        let lines = s.focus_units[index].lines.iter().map(|r| r.y-32.0).collect::<Vec<_>>();
        if let Some(delta) = rebook_app_core::focus_navigation::line_aligned_overflow_target(
            0.0,
            top - 32.0,
            bottom - height + 32.0,
            height * 0.7,
            forward,
            &lines,
        ) {
            return Some((
                index,
                FocusTarget {
                    delta,
                    bounds: s.focus_units[index].paint_bounds(),
                },
            ));
        }
    }
    let next = if forward {
        index.checked_add(1).filter(|i| *i < s.focus_units.len())?
    } else {
        index.checked_sub(1)?
    };
    let (top, bottom) = bounds(&s.focus_units[next]);
    let delta = if bottom - top > height - 64.0 {
        if forward {
            top - 32.0
        } else {
            bottom - height + 32.0
        }
    } else {
        (top + bottom) / 2.0 - height / 2.0
    };
    Some((
        next,
        FocusTarget {
            delta,
            bounds: s.focus_units[next].paint_bounds(),
        },
    ))
}
fn strip_size(s: &ReaderState) -> (u32, u32, f32, f32) {
    let v = s.session.viewport();
    let scale = v.raster_scale;
    let width = (v.width as f32 * scale).ceil() as u32;
    let viewport = (v.height as f32 * scale).ceil() as u32;
    let height = (RASTER_BUDGET / (width as usize * 4))
        .max(viewport as usize)
        .min(viewport as usize * 3) as u32;
    let pad = (height - viewport) / 2;
    (
        width,
        viewport + 2 * pad,
        -(pad as f32) / scale,
        (viewport + 2 * pad) as f32 / scale,
    )
}
fn build_cache(s: &mut ReaderState) -> Result<Cache> {
    let (width, height, top, logical_height) = strip_size(s);
    let slices = window(s, top, top + logical_height)?;
    let specs=interactions::focus_specs(s)?;
    let mut units = Vec::<interactions::FocusUnit>::new();
    let mut regions = Vec::new();
    for p in &slices {
        for mut unit in interactions::focus_units_on(&specs, &p.page) {
            shift_bounds(&mut unit.bounds, p.origin - p.top);
            shift_bounds(&mut unit.lines, p.origin - p.top);
            shift_bounds(&mut unit.headings, p.origin - p.top);
            if let Some(old) = units.iter_mut().find(|u| u.ranges == unit.ranges) {
                old.bounds.extend(unit.bounds);
                old.lines.extend(unit.lines);
                old.headings.extend(unit.headings);
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
                source: serde_json::to_string(&fragment.range).map_err(error)?,
                bounds: rects
                    .into_iter()
                    .map(|r| Rect {
                        x: r.x0 as f32,
                        y: r.y0 as f32 + p.top - p.origin,
                        width: r.width() as f32,
                        height: r.height() as f32,
                    })
                    .collect(),
            });
        }
    }
    let scale = s.session.viewport().raster_scale;
    let mut rasterizer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    rasterizer.render_to_vec(
        |scene| {
            let paper = s.session.style().background;
            scene.fill(
                Fill::NonZero,
                Affine::scale(f64::from(scale)),
                Color::from_rgba8(paper.red, paper.green, paper.blue, 255),
                None,
                &KRect::new(
                    0.0,
                    0.0,
                    f64::from(s.session.viewport().width),
                    f64::from(logical_height),
                ),
            );
            for (index, p) in slices.iter().enumerate() {
                if p.top > top + logical_height || p.top + p.height < top {
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
                                f64::from(bottom - top),
                                f64::from(bridge.x + bridge.width),
                                f64::from(next.top - top),
                            ),
                        );
                    }
                }
                scene.push_clip_layer(
                    Fill::NonZero,
                    Affine::scale(f64::from(scale)),
                    &KRect::new(
                        0.0,
                        f64::from(p.top - top),
                        f64::from(s.session.viewport().width),
                        f64::from(p.top + p.height - top),
                    ),
                );
                p.page
                    .paint_scaled_at(scene, scale, 0.0, p.top - p.origin - top);
                p.page.paint_footnote_icons_transformed(
                    scene,
                    reference_ink(s),
                    Affine::scale(f64::from(scale))
                        * Affine::translate((0.0, f64::from(p.top - p.origin - top))),
                );
                scene.pop_layer();
            }
        },
        &mut pixels,
    );
    let raster = MobilePage::new(width, height, pixels, PageInfo::default());
    let units=interactions::arrange_focus_units(units,s.session.viewport().height as f32);
    Ok(Cache {
        raster,
        distance: s.scroll_distance,
        top,
        height: logical_height,
        slices,
        units,
        regions,
    })
}
pub(crate) fn render(s: &mut ReaderState) -> Result<Arc<MobilePage>> {
    render_with_progress(s,true)
}
fn render_with_progress(s:&mut ReaderState,persist:bool)->Result<Arc<MobilePage>> {
    if s.scroll_center {
        let distance = s.scroll_distance;
        let previous = s.scroll_slices.clone();
        center(s)?;
        if let Some(slice) = previous.iter().find(|p| p.position == position(s)) {
            s.scroll_distance = distance + f64::from(s.scroll_offset + slice.top);
        } else if !previous.is_empty() {
            invalidate(s);
        }
    }
    let viewport = s.session.viewport();
    let reuse = s.scroll_cache.as_ref().is_some_and(|cache| {
        let delta = (s.scroll_distance - cache.distance) as f32;
        let guard = (-cache.top * 0.2).min(64.0);
        let page = s.session.current_page_arc();
        delta >= cache.top + guard
            && delta + viewport.height as f32 <= cache.top + cache.height - guard
            && cache
                .slices
                .iter()
                .any(|p| p.position == position(s) && Arc::ptr_eq(&p.page, &page))
    });
    let previous = s.scroll_cache.take();
    let cache = if reuse {
        previous.unwrap()
    } else {
        drop(previous);
        build_cache(s)?
    };
    let delta = (s.scroll_distance - cache.distance) as f32;
    s.scroll_slices = cache.slices.clone();
    for p in &mut s.scroll_slices {
        p.top -= delta;
    }
    s.focus_units = cache.units.clone();
    for u in &mut s.focus_units {
        shift_bounds(&mut u.bounds, delta);
        shift_bounds(&mut u.lines, delta);
        shift_bounds(&mut u.headings, delta);
    }
    let mut regions = cache.regions.clone();
    for r in &mut regions {
        shift_bounds(&mut r.bounds, delta);
    }
    regions.retain(|r| {
        r.bounds
            .iter()
            .any(|b| b.y < viewport.height as f32 && b.y + b.height > 0.0)
    });
    let saved = s.store.annotations_for_book(&s.book_id).map_err(error)?;
    let marks = s
        .scroll_slices
        .iter()
        .flat_map(|p| {
            saved.iter().flat_map(move |a| {
                p.page
                    .source_rects(&a.ranges)
                    .into_iter()
                    .map(move |r| Rect {
                        x: r.x0 as f32,
                        y: r.y0 as f32 + p.top - p.origin,
                        width: r.width() as f32,
                        height: r.height() as f32,
                    })
            })
        })
        .collect();
    let index = active(s);
    s.focus_anchor = s
        .focus_units
        .get(index)
        .and_then(|u| u.ranges.first().cloned());
    // Follow the focused logical page inside the current semantic reading unit.
    if let Some(unit) = s.focus_units.get(index) {
        let candidate = s
            .scroll_slices
            .iter()
            .filter_map(|p| {
                let distance = p
                    .page
                    .source_block_bounds(&unit.ranges)
                    .iter()
                    .map(|r| {
                        ((r.y0 + r.y1) as f32 / 2.0 + p.top
                            - p.origin
                            - viewport.height as f32 / 2.0)
                            .abs()
                    })
                    .min_by(f32::total_cmp)?;
                Some((p.position, p.top, distance))
            })
            .min_by(|a, b| a.2.total_cmp(&b.2));
        if let Some((target, top, _)) = candidate
            && target != position(s)
        {
            if s.session.set_visible_position(target).is_err() {
                s.session
                    .go_to_section(target.section_index)
                    .map_err(error)?;
                s.session.set_visible_position(target).map_err(error)?;
            }
            s.scroll_offset = -top;
        }
    }
    let mut info = finish_page_info(s, regions, marks, index, true, persist)?;
    info.raster_top = cache.top - delta;
    info.raster_height = cache.height;
    info.previous_focus = destination(s, index, false).map(|(_, t)| t);
    info.next_focus = destination(s, index, true).map(|(_, t)| t);
    let page = MobilePage::retained(&cache.raster, info);
    s.scroll_cache = Some(cache);
    Ok(page)
}
pub(crate) fn turn(s: &mut ReaderState, forward: bool) -> Result<Arc<MobilePage>> {
    let index = active(s);
    if let Some((next, target)) = destination(s, index, forward) {
        s.focus_anchor = s.focus_units[next].ranges.first().cloned();
        return scroll(s, target.delta, false);
    }
    // A bounded raster may end before this unit does. Keep logical page movement
    // within the unit; only an explicit turn at its edge changes semantic units.
    if step(s, forward)? {
        invalidate(s);
        s.focus_anchor = None;
        s.scroll_center = true;
    } else {
        let result = s.session.go_to_adjacent_reading_unit(if forward { PageDirection::Next } else { PageDirection::Previous }).map_err(error)?;
        if result.outcome == rebook_reader::NavigationOutcome::Moved {
            if !forward {
                if let Some(last) = s.session.current_reading_unit_pages().map_err(error)?.last() {
                    s.session.set_visible_position(last.position).map_err(error)?;
                }
            }
            let units = interactions::focus_units(s)?;
            let target = if forward { units.first() } else { units.last() };
            s.focus_anchor = target.and_then(|u| u.ranges.first().cloned());
            s.scroll_offset = 0.0;
            invalidate(s);
            s.scroll_center = true;
        }
    }
    render(s)
}

/// Horizontal focus navigation skips the whole semantic reading unit, even
/// when its next heading lives in the same XHTML spine item.
pub(crate) fn turn_unit(s: &mut ReaderState, forward: bool) -> Result<Arc<MobilePage>> {
    turn_unit_result(s,forward).map(|(page,_)|page)
}
pub(crate) fn turn_unit_result(s: &mut ReaderState, forward: bool) -> Result<(Arc<MobilePage>,bool)> {
    let moved=move_unit(s,forward)?;
    render(s).map(|page|(page,forward&&!moved))
}
fn move_unit(s:&mut ReaderState,forward:bool)->Result<bool> {
    let result=s.session.go_to_adjacent_reading_unit(if forward {PageDirection::Next}else{PageDirection::Previous}).map_err(error)?;
    if result.outcome==rebook_reader::NavigationOutcome::Moved {
        if !forward && let Some(last)=s.session.current_reading_unit_pages().map_err(error)?.last() {
            s.session.set_visible_position(last.position).map_err(error)?;
        }
        let units=interactions::focus_units(s)?;
        s.focus_anchor=(if forward {units.first()}else{units.last()}).and_then(|u|u.ranges.first().cloned());
        s.scroll_offset=0.0;
        invalidate(s);
        s.scroll_center=true;
    }
    Ok(result.outcome==rebook_reader::NavigationOutcome::Moved)
}

/// Prepare just the adjacent unit's visible viewport. The live scroll cache,
/// source anchor and persisted progress are restored even when preparation fails.
pub(crate) fn preview_unit(s:&mut ReaderState,forward:bool)->Result<Option<Arc<MobilePage>>> {
    let viewport=s.session.viewport();
    let bytes=(viewport.width as f64*f64::from(viewport.raster_scale)).ceil()*(viewport.height as f64*f64::from(viewport.raster_scale)).ceil()*4.0;
    if bytes>12.0*1024.0*1024.0 {return Ok(None);}
    let locator=s.session.current_locator();let position=position(s);
    let scroll=(s.scroll_offset,s.scroll_distance,s.scroll_context,s.scroll_center);
    let cache=s.scroll_cache.take();let slices=std::mem::take(&mut s.scroll_slices);
    let units=std::mem::take(&mut s.focus_units);let anchor=s.focus_anchor.take();
    let result=(||{
        if !move_unit(s,forward)? {return Ok(None);}
        let strip=render_with_progress(s,false)?;
        let mut info=strip.info();let viewport=s.session.viewport();
        let width=strip.width();let height=(viewport.height as f32*viewport.raster_scale).ceil() as u32;
        let start=-(info.raster_top*viewport.raster_scale).round() as i64;
        let paper=s.session.style().background;
        let mut pixels=[paper.red,paper.green,paper.blue,255].repeat(width as usize*height as usize);
        let stride=width as usize*4;
        for row in 0..height as i64 {
            let source=start+row;
            if source>=0&&source<i64::from(strip.height()) {
                let from=source as usize*stride;let to=row as usize*stride;
                pixels[to..to+stride].copy_from_slice(&strip.pixels[from..from+stride]);
            }
        }
        info.raster_top=0.0;info.raster_height=viewport.height as f32;
        Ok(Some(MobilePage::new(width,height,pixels,info)))
    })();
    let restored=restore_position(s,&locator,position);
    s.scroll_cache=cache;s.scroll_slices=slices;s.focus_units=units;s.focus_anchor=anchor;
    (s.scroll_offset,s.scroll_distance,s.scroll_context,s.scroll_center)=scroll;
    restored?;result
}

fn scroll(s: &mut ReaderState, delta: f32, follow_center: bool) -> Result<Arc<MobilePage>> {
    if !delta.is_finite() {
        return Err(error("滚动距离无效"));
    }
    let start_offset = s.scroll_offset;
    let mut travel = 0.0;
    s.scroll_offset += delta.clamp(
        -4.0 * s.session.viewport().height as f32,
        4.0 * s.session.viewport().height as f32,
    );
    for _ in 0..8 {
        let (_, height) = current_extent(s);
        if s.scroll_offset >= height {
            if !step(s, true)? {
                s.scroll_offset = s.scroll_offset.min(height - s.session.viewport().height as f32 / 2.0);
                break;
            }
            let advance = height + s.session.current_page().leading_gap();
            s.scroll_offset -= advance;
            travel += advance;
        } else if s.scroll_offset < -(s.session.viewport().height as f32) / 2.0 {
            let gap = s.session.current_page().leading_gap();
            if !step(s, false)? {
                s.scroll_offset = s
                    .scroll_offset
                    .max(-(s.session.viewport().height as f32) / 2.0);
                break;
            }
            let advance = current_extent(s).1 + gap;
            s.scroll_offset += advance;
            travel -= advance;
        } else {
            break;
        }
    }
    s.scroll_distance += f64::from(travel + s.scroll_offset - start_offset);
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
