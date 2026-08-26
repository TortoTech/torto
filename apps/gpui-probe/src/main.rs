//! Isolated GPUI consumer for Torto's backend-neutral `LayoutFrame` contract.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Application, Bounds, ClipboardItem, Context, FocusHandle, KeyBinding,
    MouseButton, MouseDownEvent, Render, ScrollHandle, Window, WindowBounds, WindowOptions,
    WindowTextSystem, actions, div, prelude::*, px, rgb, size,
};
use rebook_gpui_renderer::{GpuiFramePresenter, GpuiTextEngine};
use rebook_layout::{
    LayoutEngine, LayoutFrame, LayoutViewport, RasterImage, ReaderFontBlob, ReaderStyle,
    frame::{FrameTextCursor, FrameTextSelection},
    text::TextEngine,
};
use rebook_publication::{
    Block, BlockStyle, Book, BookSource, ImageBlock, ImageStyle, Inline, Metadata,
    PublicationError, PublicationId, PublicationUrl, RasterResource, Resource, Section,
    SourceAnchor, SourceRange, SpineItemId, TextBlock, TextBlockKind, TextRun, TextStyle,
};

#[cfg(test)]
use rebook_layout::PageItem;
#[cfg(any(test, debug_assertions))]
use rebook_layout::text::legacy_parley::LegacyParleyTextEngine;

const PAGE_PADDING: f32 = 48.0;
const MIN_PAGE_WIDTH: f32 = 360.0;
const MAX_PAGE_WIDTH: f32 = 800.0;
const PROBE_PAGE_HEIGHT: u32 = 2_000;
const PROBE_SERIF_FONT: &[u8] = include_bytes!("../../../assets/fonts/Bitter-wght.ttf");

actions!(
    torto_gpui_probe,
    [
        MoveLeft,
        MoveRight,
        SelectLeft,
        SelectRight,
        SelectAll,
        Copy
    ]
);

#[derive(Clone, Copy)]
enum CursorDirection {
    Previous,
    Next,
}

struct Probe {
    source: ProbeSource,
    style: ReaderStyle,
    engine: LayoutEngine<GpuiTextEngine>,
    frame: Arc<LayoutFrame>,
    frame_width: f32,
    selection_anchor: Option<FrameTextCursor>,
    selection_focus: Option<FrameTextCursor>,
    selection: Option<FrameTextSelection>,
    status: String,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    presenter: GpuiFramePresenter,
}

impl Probe {
    fn new(width: f32, text_system: Arc<WindowTextSystem>, focus_handle: FocusHandle) -> Self {
        let source = ProbeSource::new(sample_raster());
        let style = probe_style();
        let mut engine = probe_layout_engine(GpuiTextEngine::new(text_system));
        let frame = layout_probe_frame(&mut engine, &source, &style, width);
        #[cfg(debug_assertions)]
        assert_gpui_frame_parity(&source, &style, width, &frame);
        Self {
            source,
            style,
            engine,
            frame: Arc::new(frame),
            frame_width: width,
            selection_anchor: None,
            selection_focus: None,
            selection: None,
            status: "单击正文后可用 Shift+←/→ 扩展选择，Ctrl/Cmd+C 复制".into(),
            scroll: ScrollHandle::new(),
            focus_handle,
            presenter: GpuiFramePresenter::new(),
        }
    }

    fn rebuild(&mut self, width: f32) {
        let persisted_ranges = self
            .selection
            .as_ref()
            .map(|selection| selection.ranges.clone());
        let frame = layout_probe_frame(&mut self.engine, &self.source, &self.style, width);
        #[cfg(debug_assertions)]
        assert_gpui_frame_parity(&self.source, &self.style, width, &frame);
        self.frame = Arc::new(frame);
        self.frame_width = width;
        self.presenter.clear();
        if let Some(ranges) = persisted_ranges
            && let Some((anchor, focus)) =
                self.frame.interaction().cursors_for_source_ranges(&ranges)
        {
            self.set_selection(anchor, focus);
            let rect_count = self
                .selection
                .as_ref()
                .map_or(0, |selection| selection.rects.len());
            self.status = format!("resize 后由 SourceRange 恢复选择：{rect_count} 个几何片段");
        } else {
            self.selection_anchor = None;
            self.selection_focus = None;
            self.selection = None;
        }
    }

    fn set_selection(&mut self, anchor: FrameTextCursor, focus: FrameTextCursor) {
        self.selection_anchor = Some(anchor);
        self.selection_focus = Some(focus);
        self.selection = self.frame.interaction().selection_between(anchor, focus);
        if let Some(selection) = &self.selection {
            self.status = format!(
                "已选择 {} 个字符 · {} 个 SourceRange",
                selection.quote.chars().count(),
                selection.ranges.len()
            );
        } else {
            self.status = "光标已移动；按住 Shift 配合左右方向键选择".into();
        }
    }

    fn move_cursor(&mut self, direction: CursorDirection, extend: bool, cx: &mut Context<Self>) {
        let Some(current) = self.selection_focus.or_else(|| match direction {
            CursorDirection::Previous => self.frame.interaction().last_cursor(),
            CursorDirection::Next => self.frame.interaction().first_cursor(),
        }) else {
            return;
        };
        let target = if !extend && self.selection.is_some() {
            match direction {
                CursorDirection::Previous => self.selection_anchor.min(self.selection_focus),
                CursorDirection::Next => self.selection_anchor.max(self.selection_focus),
            }
            .unwrap_or(current)
        } else {
            match direction {
                CursorDirection::Previous => self.frame.interaction().previous_cursor(current),
                CursorDirection::Next => self.frame.interaction().next_cursor(current),
            }
            .unwrap_or(current)
        };
        let anchor = if extend {
            self.selection_anchor.unwrap_or(current)
        } else {
            target
        };
        self.set_selection(anchor, target);
        cx.notify();
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(CursorDirection::Previous, false, cx);
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(CursorDirection::Next, false, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(CursorDirection::Previous, true, cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(CursorDirection::Next, true, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let interaction = self.frame.interaction();
        let (Some(anchor), Some(focus)) = (interaction.first_cursor(), interaction.last_cursor())
        else {
            return;
        };
        self.set_selection(anchor, focus);
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = &self.selection else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(selection.quote.clone()));
        self.status = format!("已复制 {} 个字符", selection.quote.chars().count());
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some(page_bounds) = self.scroll.bounds_for_item(0) else {
            return;
        };
        let x = f32::from(event.position.x) - f32::from(page_bounds.origin.x);
        let y = f32::from(event.position.y) - f32::from(page_bounds.origin.y);
        let Some(hit) = self.frame.interaction().hit_test_text(x, y, true) else {
            self.selection_anchor = None;
            self.selection_focus = None;
            self.selection = None;
            self.status = "未命中文本".into();
            cx.notify();
            return;
        };
        let hit_cursor = FrameTextCursor::new(hit.region_index, hit.byte_index);
        let (anchor, focus) = if event.modifiers.shift {
            let anchor = self.selection_anchor.unwrap_or(hit_cursor);
            let focus = if hit_cursor < anchor {
                FrameTextCursor::new(hit.region_index, hit.cluster_start)
            } else {
                FrameTextCursor::new(hit.region_index, hit.cluster_end)
            };
            (anchor, focus)
        } else {
            (
                FrameTextCursor::new(hit.region_index, hit.cluster_start),
                FrameTextCursor::new(hit.region_index, hit.cluster_end),
            )
        };
        self.set_selection(anchor, focus);
        cx.notify();
    }

    fn page_elements(&mut self) -> Vec<AnyElement> {
        let selections = self
            .selection
            .as_ref()
            .map_or(&[][..], |selection| selection.ranges.as_slice());
        self.presenter.elements(self.frame.as_ref(), selections)
    }
}

impl Render for Probe {
    #[allow(
        clippy::cast_precision_loss,
        reason = "probe viewport dimensions are capped at 800 DIP"
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let available =
            (f32::from(window.bounds().size.width) - 64.0).clamp(MIN_PAGE_WIDTH, MAX_PAGE_WIDTH);
        if (available - self.frame_width).abs() > 1.0 {
            self.rebuild(available);
        }
        let anchors = self.frame.source_anchors();
        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_6()
            .py_3()
            .bg(rgb(0x00F5_F7F6))
            .border_b_1()
            .border_color(rgb(0x00D1_D7DE))
            .child("Torto GPUI LayoutFrame Probe")
            .child(div().text_sm().text_color(rgb(0x005F_6B66)).child(format!(
                "{} · anchors {:?} → {:?}",
                self.status,
                anchors.map(|anchors| anchors.first.text_offset),
                anchors.map(|anchors| anchors.last.text_offset)
            )));
        let page = div()
            .relative()
            .w(px(self.frame.viewport.width as f32))
            .h(px(self.frame.viewport.height as f32))
            .bg(rgb(0x00FF_FCF7))
            .shadow_md()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .children(self.page_elements());

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x00E9_ECEA))
            .child(header)
            .child(
                div()
                    .id("layout-frame-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p_6()
                    .child(page),
            )
    }
}

fn probe_style() -> ReaderStyle {
    ReaderStyle {
        horizontal_margin: PAGE_PADDING,
        top_margin: 36.0,
        bottom_margin: 36.0,
        ..ReaderStyle::default()
    }
}

fn probe_layout_engine<E: TextEngine>(text_engine: E) -> LayoutEngine<E> {
    let mut engine = LayoutEngine::with_text_engine(text_engine);
    engine.register_fonts([ReaderFontBlob::from_static(PROBE_SERIF_FONT)]);
    engine
}

#[cfg(test)]
fn build_legacy_frame(width: f32) -> LayoutFrame {
    let source = ProbeSource::new(sample_raster());
    let style = probe_style();
    let mut engine = probe_layout_engine(LegacyParleyTextEngine::default());
    layout_probe_frame(&mut engine, &source, &style, width)
}

#[cfg(debug_assertions)]
fn assert_gpui_frame_parity(
    source: &ProbeSource,
    style: &ReaderStyle,
    width: f32,
    candidate: &LayoutFrame,
) {
    let mut legacy = probe_layout_engine(LegacyParleyTextEngine::default());
    let reference = layout_probe_frame(&mut legacy, source, style, width);
    assert_source_parity(&reference, candidate);
    assert!(
        candidate.items.iter().any(|item| {
            let rebook_layout::PageItem::Text(text) = item else {
                return false;
            };
            if text
                .source
                .as_ref()
                .map(|source| source.start.node.as_str())
                != Some("list")
            {
                return false;
            }
            text.lines.iter().any(|line_id| {
                text.layout.line(line_id).is_some_and(|line| {
                    line.items.iter().any(|item| {
                        matches!(item, rebook_layout::text::TextPaintItem::NativeRun(_))
                    })
                })
            })
        }),
        "registered Bitter bytes and hanging list geometry must remain on the GPUI-native path"
    );
}

#[cfg(any(test, debug_assertions))]
fn assert_source_parity(reference: &LayoutFrame, candidate: &LayoutFrame) {
    assert_eq!(
        coalesced_text_coverage(reference),
        coalesced_text_coverage(candidate),
        "text engines changed authored source coverage"
    );
    assert_eq!(
        reference.source_anchors(),
        candidate.source_anchors(),
        "text engines changed the frame's durable source anchors"
    );
}

#[cfg(any(test, debug_assertions))]
fn coalesced_text_coverage(frame: &LayoutFrame) -> Vec<SourceRange> {
    let mut coverage: Vec<SourceRange> = Vec::new();
    for range in frame.line_source_ranges() {
        if let Some(previous) = coverage.last_mut()
            && previous.end == range.start
        {
            previous.end = range.end;
        } else {
            coverage.push(range);
        }
    }
    coverage
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "probe geometry is clamped to positive dimensions below 1,024 DIP"
)]
fn layout_probe_frame<E: TextEngine>(
    engine: &mut LayoutEngine<E>,
    source: &ProbeSource,
    style: &ReaderStyle,
    width: f32,
) -> LayoutFrame {
    let page_width = width.round().clamp(MIN_PAGE_WIDTH, MAX_PAGE_WIDTH);
    let viewport = LayoutViewport::new(page_width as u32, PROBE_PAGE_HEIGHT).unwrap();
    let mut frames = engine
        .layout_blocks(source, &source.blocks, viewport, style)
        .expect("probe Reading IR must lay out")
        .into_frames();
    assert_eq!(frames.len(), 1, "probe fixture must fit one tall frame");
    frames.remove(0)
}

struct ProbeSource {
    book: Book,
    blocks: Vec<Block>,
    raster: RasterImage,
    image_href: PublicationUrl,
}

impl ProbeSource {
    fn new(raster: RasterImage) -> Self {
        let image_href = PublicationUrl::parse("images/probe.png").unwrap();
        let mut source_offset = 0_u64;
        let mut blocks = Vec::new();
        blocks.push(Block::Text(probe_text_block(
            TextBlockKind::Heading(1),
            "A LayoutFrame Is the Product Boundary",
            "title",
            &mut source_offset,
            true,
        )));
        for (index, paragraph) in SAMPLE_PARAGRAPHS.iter().enumerate() {
            blocks.push(Block::Text(probe_text_block(
                TextBlockKind::Paragraph,
                paragraph,
                &format!("p{index}"),
                &mut source_offset,
                false,
            )));
            if index == 1 {
                blocks.push(Block::Image(ImageBlock {
                    href: image_href.clone(),
                    alt: "Backend-neutral layout pipeline".into(),
                    style: ImageStyle::default(),
                    source: Some(source_range("image", source_offset, 0)),
                    text_layer: None,
                }));
            }
        }
        blocks.push(Block::Text(probe_text_block(
            TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 0,
                marker_visible: true,
            },
            SAMPLE_LIST_ITEM,
            "list",
            &mut source_offset,
            false,
        )));
        Self {
            book: Book {
                id: PublicationId::new("gpui-probe").unwrap(),
                metadata: Metadata {
                    title: "GPUI layout probe".into(),
                    languages: vec!["en".into()],
                    ..Metadata::default()
                },
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
            blocks,
            raster,
            image_href,
        }
    }
}

impl BookSource for ProbeSource {
    fn book(&self) -> &Book {
        &self.book
    }

    fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
        Err(PublicationError::ResourceNotFound("probe section".into()))
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        Err(PublicationError::ResourceNotFound(href.to_string()))
    }

    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        Ok((href == &self.image_href).then(|| RasterResource {
            width: self.raster.width,
            height: self.raster.height,
            pixels: Arc::clone(&self.raster.pixels),
        }))
    }
}

fn probe_text_block(
    kind: TextBlockKind,
    content: &str,
    node: &str,
    source_offset: &mut u64,
    heading: bool,
) -> TextBlock {
    let length = u64::try_from(content.chars().count()).unwrap_or(u64::MAX);
    let source = source_range(node, *source_offset, length);
    *source_offset = source_offset.saturating_add(length);
    TextBlock {
        kind,
        content: vec![Inline::Text(TextRun {
            text: content.into(),
            style: TextStyle {
                bold: heading,
                size_scale: if heading { 1.6 } else { 1.0 },
                ..TextStyle::default()
            },
            link: None,
        })],
        style: BlockStyle {
            margin_after: if heading { 24.0 } else { 20.0 },
            line_height: if heading { 1.2 } else { 1.5 },
            ..BlockStyle::default()
        },
        source: Some(source),
    }
}

fn source_range(node: &str, start: u64, length: u64) -> SourceRange {
    let spine = SpineItemId::new("probe").expect("static probe spine id is valid");
    SourceRange {
        start: SourceAnchor {
            spine: spine.clone(),
            node: node.into(),
            text_offset: start,
        },
        end: SourceAnchor {
            spine,
            node: node.into(),
            text_offset: start.saturating_add(length),
        },
    }
}

fn sample_raster() -> RasterImage {
    const WIDTH: u32 = 320;
    const HEIGHT: u32 = 160;
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            pixels.extend_from_slice(&[
                70_u8.saturating_add(u8::try_from(x / 4).unwrap_or(u8::MAX)),
                128_u8.saturating_add(u8::try_from(y / 6).unwrap_or(u8::MAX)),
                120,
                255,
            ]);
        }
    }
    RasterImage {
        width: WIDTH,
        height: HEIGHT,
        pixels: pixels.into(),
    }
}

const SAMPLE_PARAGRAPHS: [&str; 6] = [
    "Torto owns content interpretation, line-break decisions, pagination, and stable source mapping. GPUI receives an immutable frame and is responsible only for application UI, interaction dispatch, text-system services, and final paint.",
    "This probe deliberately rebuilds the frame whenever the window width changes. The same SourceRange is then resolved against the new frame, demonstrating that a resize changes geometry without invalidating reading position or annotations.",
    "Each shaped line is represented by backend-neutral metrics and a store-local line identifier. Paragraph flow chooses half-open line ranges for a region before any GPUI element is created, so the UI toolkit cannot accidentally become the pagination authority.",
    "The image above is carried by the same LayoutFrame as prose. Fixed-layout PDF and CBZ pages can join the pipeline at this boundary without passing through the complete reflow algorithm.",
    "Mixed-script prose 也通过同一套 UAX #14 断点进入全局 Flow；GPUI 只负责字体回退和 shaping，而不是决定分页。",
    "Click a word, resize the window, and scroll through the document. The status bar reports source offsets while the translucent rectangle is regenerated from the preserved SourceRange.",
];

const SAMPLE_LIST_ITEM: &str = "A hanging list item keeps its marker on the full-width first line while continuation lines use a narrower measure selected by the same Torto-owned line-breaking policy.";

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("left", MoveLeft, None),
            KeyBinding::new("right", MoveRight, None),
            KeyBinding::new("shift-left", SelectLeft, None),
            KeyBinding::new("shift-right", SelectRight, None),
            KeyBinding::new("secondary-a", SelectAll, None),
            KeyBinding::new("secondary-c", Copy, None),
        ]);
        // Start below the 800 DIP page cap so maximizing the window exercises
        // a real width-driven frame rebuild during the manual probe.
        let bounds = Bounds::centered(None, size(px(700.0), px(760.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let width = (f32::from(window.bounds().size.width) - 64.0)
                    .clamp(MIN_PAGE_WIDTH, MAX_PAGE_WIDTH);
                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    window.focus(&focus_handle);
                    Probe::new(width, Arc::clone(window.text_system()), focus_handle)
                })
            },
        )
        .expect("GPUI probe window should open");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_nonempty_text_source(frame: &LayoutFrame) -> SourceRange {
        frame
            .items
            .iter()
            .find_map(|item| match item {
                PageItem::Text(text) => text.source.clone(),
                PageItem::Image(_)
                | PageItem::Separator(_)
                | PageItem::Quote(_)
                | PageItem::Table(_) => None,
            })
            .expect("probe frame must expose a text source range")
    }

    #[test]
    fn probe_frame_contains_title_paragraphs_and_image() {
        let frame = build_legacy_frame(640.0);
        let text_count = frame
            .items
            .iter()
            .filter(|item| matches!(item, PageItem::Text(_)))
            .count();
        let image_count = frame
            .items
            .iter()
            .filter(|item| matches!(item, PageItem::Image(_)))
            .count();

        assert_eq!(text_count, SAMPLE_PARAGRAPHS.len() + 2);
        assert_eq!(image_count, 1);
        let anchors = frame
            .source_anchors()
            .expect("probe frame must have anchors");
        assert!(anchors.last.text_offset > anchors.first.text_offset);
    }

    #[test]
    fn source_selection_survives_a_width_change() {
        let narrow = build_legacy_frame(520.0);
        let source = first_nonempty_text_source(&narrow);
        let one_character = SourceRange {
            start: source.start.clone(),
            end: SourceAnchor {
                text_offset: source.start.text_offset + 1,
                ..source.end.clone()
            },
        };
        assert!(
            !narrow
                .interaction()
                .source_rects(std::slice::from_ref(&one_character))
                .is_empty()
        );

        let wide = build_legacy_frame(800.0);
        assert!(!wide.interaction().source_rects(&[one_character]).is_empty());
    }

    #[test]
    fn source_coverage_and_anchors_survive_different_soft_breaks() {
        let narrow = build_legacy_frame(480.0);
        let wide = build_legacy_frame(800.0);

        assert_source_parity(&narrow, &wide);
        assert_ne!(
            narrow.line_source_ranges(),
            wide.line_source_ranges(),
            "fixture must produce different line boundaries"
        );
    }

    #[test]
    fn retained_line_ranges_are_valid_utf8_and_non_overlapping() {
        let frame = build_legacy_frame(640.0);
        for item in &frame.items {
            let PageItem::Text(text) = item else {
                continue;
            };
            let mut previous_end = 0;
            for line_id in text.lines.iter() {
                let line = text
                    .layout
                    .line(line_id)
                    .expect("line id belongs to its store");
                assert!(text.text.get(line.text_range.clone()).is_some());
                assert!(line.text_range.start >= previous_end);
                previous_end = line.text_range.end;
            }
        }
    }
}
