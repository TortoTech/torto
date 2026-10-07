//! Shared client title bar. Native hit testing uses the same rectangles as painting.
use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2};

pub(crate) const HEIGHT: f32 = 44.0;
const BUTTON_WIDTH: f32 = 46.0;

#[derive(Clone, Copy, Default)]
pub(crate) struct WindowState {
    pub fullscreen: bool,
    pub maximized: bool,
    pub header_hovered: bool,
    pub hovered_button: Option<usize>,
    pub pressed_button: Option<usize>,
}

#[derive(Clone, Default)]
pub(crate) struct Geometry {
    pub header: Option<Rect>,
    pub excluded: Vec<Rect>,
    pub buttons: [Option<Rect>; 3],
    pub drag_enabled: bool,
    pub background: Option<Color32>,
    pub overlay_dim: f32,
    pub header_masks: Vec<(Rect, f32)>,
}

fn state_id() -> egui::Id {
    egui::Id::new("native-window-state")
}
fn geometry_id() -> egui::Id {
    egui::Id::new("native-window-geometry")
}

fn controls_layer() -> egui::LayerId {
    // Native caption controls must stay above app overlays, including Tooltip
    // image previews. Sharing their order lets a newly opened preview dim them twice.
    egui::LayerId::new(egui::Order::Debug, egui::Id::new("window-caption-buttons"))
}

pub(crate) fn set_state(ctx: &egui::Context, state: WindowState) {
    ctx.data_mut(|data| data.insert_temp(state_id(), state));
}
pub(crate) fn state(ctx: &egui::Context) -> WindowState {
    ctx.data(|data| data.get_temp(state_id()))
        .unwrap_or_default()
}
pub(crate) fn begin_frame(ctx: &egui::Context) {
    ctx.data_mut(|data| data.insert_temp(geometry_id(), Geometry::default()));
}
pub(crate) fn geometry(ctx: &egui::Context) -> Geometry {
    ctx.data(|data| data.get_temp(geometry_id()))
        .unwrap_or_default()
}
pub(crate) fn body_rect(ctx: &egui::Context) -> Rect {
    let mut rect = ctx.content_rect();
    if let Some(header) = geometry(ctx).header {
        rect.min.y = rect.top().max(header.bottom());
    }
    rect
}

/// Create a child UI with a real layout boundary, independent of the parent's minimum size.
pub(crate) fn bounded_ui<R>(
    ui: &mut egui::Ui,
    rect: Rect,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(rect));
        content(ui)
    })
    .inner
}

pub(crate) fn reserve_width(ctx: &egui::Context) -> f32 {
    if state(ctx).fullscreen {
        0.0
    } else {
        BUTTON_WIDTH * 3.0
    }
}
pub(crate) fn header(ctx: &egui::Context, rect: Rect) {
    let drag_enabled = !state(ctx).fullscreen;
    ctx.data_mut(|data| {
        let geometry = data.get_temp_mut_or_default::<Geometry>(geometry_id());
        geometry.header = Some(rect);
        geometry.drag_enabled = drag_enabled;
    });
}
pub(crate) fn exclude(ctx: &egui::Context, rect: Rect) {
    ctx.data_mut(|data| {
        let geometry = data.get_temp_mut_or_default::<Geometry>(geometry_id());
        if geometry
            .header
            .is_some_and(|header| header.intersects(rect))
        {
            geometry.excluded.push(rect);
        }
    });
}
pub(crate) fn set_background(ctx: &egui::Context, color: Color32) {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Geometry>(geometry_id())
            .background = Some(color)
    });
}

/// Match the app backdrop over caption buttons, which must remain above modal layers.
pub(crate) fn dim_controls(ctx: &egui::Context, opacity: f32) {
    ctx.data_mut(|data| {
        let geometry = data.get_temp_mut_or_default::<Geometry>(geometry_id());
        geometry.overlay_dim = 1.0 - (1.0 - geometry.overlay_dim) * (1.0 - opacity.clamp(0.0, 1.0));
        geometry.drag_enabled = false;
    });
}

pub(crate) fn dim_header(ctx: &egui::Context, opacity: f32, left: f32) {
    ctx.data_mut(|data| {
        let geometry = data.get_temp_mut_or_default::<Geometry>(geometry_id());
        if let Some(mut rect) = geometry.header {
            rect.min.x = left.clamp(rect.left(), rect.right());
            geometry.header_masks.push((rect, opacity.clamp(0.0, 1.0)));
        }
        geometry.drag_enabled = false;
    });
}

fn paint_header_masks(ctx: &egui::Context) {
    let geometry = geometry(ctx);
    if geometry.header_masks.is_empty() {
        return;
    }
    let Some(header) = geometry.header else {
        return;
    };
    // Append header scrims to the controls' paint layer so they cannot reorder
    // independently when a caption button is hovered or pressed.
    let painter = ctx.layer_painter(controls_layer()).with_clip_rect(header);
    for (rect, opacity) in geometry.header_masks {
        painter.rect_filled(rect, 0.0, Color32::BLACK.gamma_multiply(opacity));
    }
}

pub(crate) fn block_drag(ctx: &egui::Context) {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Geometry>(geometry_id())
            .drag_enabled = false
    });
}

pub(crate) fn fallback_header(ui: &mut egui::Ui) {
    egui::Panel::top("window-titlebar")
        .exact_size(HEIGHT)
        .frame(egui::Frame::new().fill(crate::ui::palette().background))
        .show(ui, |ui| {
            header(ui.ctx(), ui.max_rect());
            ui.painter().text(
                ui.max_rect().left_center() + Vec2::new(16.0, 0.0),
                egui::Align2::LEFT_CENTER,
                "Torto",
                egui::FontId::proportional(14.0),
                crate::ui::palette().text,
            );
        });
}

pub(crate) fn paint_controls(ctx: &egui::Context) {
    let state = state(ctx);
    if state.fullscreen {
        paint_header_masks(ctx);
        return;
    }
    let rect = geometry(ctx).header.unwrap_or_else(|| {
        Rect::from_min_size(
            ctx.content_rect().min,
            Vec2::new(ctx.content_rect().width(), HEIGHT),
        )
    });
    let origin = Pos2::new(rect.right() - BUTTON_WIDTH * 3.0, rect.top());
    let layer = controls_layer();
    egui::Area::new(layer.id)
        .fade_in(false)
        .order(layer.order)
        .fixed_pos(origin)
        .constrain(false)
        .default_size(Vec2::new(BUTTON_WIDTH * 3.0, rect.height()))
        .show(ctx, |ui| {
            ui.painter().rect_filled(
                Rect::from_min_size(origin, Vec2::new(BUTTON_WIDTH * 3.0, rect.height())),
                0.0,
                geometry(ctx)
                    .background
                    .unwrap_or(crate::ui::palette().background),
            );
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.horizontal(|ui| {
                for index in 0..3 {
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(BUTTON_WIDTH, rect.height()),
                        Sense::click(),
                    );
                    exclude(ctx, rect);
                    ctx.data_mut(|data| {
                        data.get_temp_mut_or_default::<Geometry>(geometry_id())
                            .buttons[index] = Some(rect);
                    });
                    let hovered = response.hovered() || (state.hovered_button == Some(index));
                    let pressed = response.is_pointer_button_down_on()
                        || (state.pressed_button == Some(index));
                    if hovered || pressed {
                        ui.painter().rect_filled(
                            rect,
                            0.0,
                            if index == 2 {
                                Color32::from_rgb(196, 43, 28)
                            } else {
                                crate::ui::palette().text.gamma_multiply(if pressed {
                                    0.18
                                } else {
                                    0.09
                                })
                            },
                        );
                    }
                    let color = if index == 2 && hovered {
                        Color32::WHITE
                    } else {
                        crate::ui::palette().text
                    };
                    let stroke = Stroke::new(1.0, color);
                    let center = rect.center();
                    match index {
                        0 => {
                            ui.painter().line_segment(
                                [center - Vec2::new(5.0, 0.0), center + Vec2::new(5.0, 0.0)],
                                stroke,
                            );
                        }
                        1 if state.maximized => {
                            let back = Rect::from_center_size(
                                center + Vec2::new(1.5, -1.5),
                                Vec2::splat(8.0),
                            );
                            ui.painter()
                                .line_segment([back.left_top(), back.right_top()], stroke);
                            ui.painter()
                                .line_segment([back.right_top(), back.right_bottom()], stroke);
                            ui.painter().rect_stroke(
                                Rect::from_center_size(
                                    center + Vec2::new(-1.0, 1.0),
                                    Vec2::splat(8.0),
                                ),
                                0.0,
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                        1 => {
                            ui.painter().rect_stroke(
                                Rect::from_center_size(center, Vec2::splat(10.0)),
                                0.0,
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                        _ => {
                            ui.painter().line_segment(
                                [center - Vec2::splat(5.0), center + Vec2::splat(5.0)],
                                stroke,
                            );
                            ui.painter().line_segment(
                                [center + Vec2::new(-5.0, 5.0), center + Vec2::new(5.0, -5.0)],
                                stroke,
                            );
                        }
                    }
                    if response.clicked() {
                        match index {
                            0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                            1 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(
                                !state.maximized,
                            )),
                            _ => ctx.data_mut(|data| {
                                data.insert_temp(egui::Id::new("window-close-requested"), true);
                            }),
                        }
                    }
                }
            });
            let dim = geometry(ctx).overlay_dim;
            if dim > 0.0 {
                ui.painter().rect_filled(
                    Rect::from_min_size(origin, Vec2::new(BUTTON_WIDTH * 3.0, rect.height())),
                    0.0,
                    Color32::BLACK.gamma_multiply(dim),
                );
            }
        });
    paint_header_masks(ctx);
}

pub(crate) fn take_close_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|data| data.remove_temp::<bool>(egui::Id::new("window-close-requested")))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caption_rectangles_stay_inside_header_across_widths_and_fullscreen() {
        let ctx = egui::Context::default();
        for width in [720.0, 1200.0, 1920.0] {
            let band = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT));
            for _ in 0..2 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 800.0))),
                        ..Default::default()
                    },
                    |_| {
                        begin_frame(&ctx);
                        header(&ctx, band);
                        paint_controls(&ctx);
                    },
                );
                output.textures_delta.clear();
            }
            let geometry = geometry(&ctx);
            assert_eq!(
                geometry.buttons[0],
                Some(Rect::from_min_size(
                    Pos2::new(width - 138.0, 0.0),
                    Vec2::new(46.0, HEIGHT)
                ))
            );
            assert_eq!(
                geometry.buttons[1],
                Some(Rect::from_min_size(
                    Pos2::new(width - 92.0, 0.0),
                    Vec2::new(46.0, HEIGHT)
                ))
            );
            assert_eq!(
                geometry.buttons[2],
                Some(Rect::from_min_size(
                    Pos2::new(width - 46.0, 0.0),
                    Vec2::new(46.0, HEIGHT)
                ))
            );
            assert!(
                geometry
                    .buttons
                    .into_iter()
                    .all(|rect| band.contains_rect(rect.unwrap()))
            );
        }
        set_state(
            &ctx,
            WindowState {
                fullscreen: true,
                ..Default::default()
            },
        );
        let mut output = ctx.run_ui(Default::default(), |_| {
            begin_frame(&ctx);
            header(
                &ctx,
                Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, HEIGHT)),
            );
            paint_controls(&ctx);
        });
        output.textures_delta.clear();
        assert!(geometry(&ctx).buttons.iter().all(Option::is_none));
    }
    #[test]
    fn bounded_header_ui_releases_parent_minimum_width() {
        for width in [720.0, 1200.0] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 520.0))),
                    ..Default::default()
                },
                |root| {
                    begin_frame(&ctx);
                    egui::Panel::top("test-caption")
                        .show_separator_line(false)
                        .frame(egui::Frame::new())
                        .exact_size(HEIGHT)
                        .show(root, |ui| {
                            let full = ui.max_rect();
                            header(ui.ctx(), full);
                            let bounds = Rect::from_min_max(
                                full.min + Vec2::new(16.0, 0.0),
                                Pos2::new(
                                    full.right() - reserve_width(ui.ctx()) - 12.0,
                                    full.bottom(),
                                ),
                            );
                            bounded_ui(ui, bounds, |ui| {
                                assert!((ui.available_width() - bounds.width()).abs() < 0.01);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let button =
                                            crate::ui::icon_button(ui, crate::ui::Icon::Settings);
                                        assert!(bounds.contains_rect(button.rect));
                                        assert!(ui.clip_rect().right() <= bounds.right());
                                    },
                                );
                            });
                        });
                    paint_controls(&ctx);
                    let geometry = geometry(&ctx);
                    let first = geometry.buttons[0].unwrap();
                    assert!(
                        geometry
                            .excluded
                            .iter()
                            .filter(|rect| rect.left() < first.left())
                            .all(|rect| rect.right() <= first.left())
                    );
                    assert_eq!(body_rect(&ctx).top(), HEIGHT);
                },
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn backdrops_cover_caption_controls_and_follow_fullscreen() {
        let ctx = egui::Context::default();
        for fullscreen in [false, true] {
            set_state(
                &ctx,
                WindowState {
                    fullscreen,
                    ..Default::default()
                },
            );
            let mut output = None;
            for _ in 0..2 {
                let mut frame = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 520.0))),
                        ..Default::default()
                    },
                    |_| {
                        begin_frame(&ctx);
                        header(
                            &ctx,
                            Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, HEIGHT)),
                        );
                        dim_header(&ctx, 0.31, 256.0);
                        dim_controls(&ctx, 0.46);
                        paint_controls(&ctx);
                        assert!(!geometry(&ctx).drag_enabled);
                    },
                );
                frame.textures_delta.clear();
                output = Some(frame);
            }
            let output = output.unwrap();
            let header_mask = Rect::from_min_max(Pos2::new(256.0, 0.0), Pos2::new(720.0, HEIGHT));
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.rect == header_mask && rect.fill == Color32::BLACK.gamma_multiply(0.31))), "drawer backdrop covers the header and native controls: {:?}", output.shapes);
            if !fullscreen {
                let controls = Rect::from_min_size(Pos2::new(582.0, 0.0), Vec2::new(138.0, HEIGHT));
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.rect == controls && rect.fill == Color32::BLACK.gamma_multiply(0.46))), "settings backdrop also covers the three caption buttons");
                let caption_index = output.shapes.iter().position(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.rect == controls && rect.fill == crate::ui::palette().background)).expect("opaque caption background");
                let mask_index = output.shapes.iter().position(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.rect == header_mask && rect.fill == Color32::BLACK.gamma_multiply(0.31))).unwrap();
                assert!(
                    caption_index < mask_index,
                    "drawer header tint must stay above the caption background"
                );
            }
        }
    }

    #[test]
    fn fullscreen_releases_buttons_and_disables_caption_dragging() {
        let ctx = egui::Context::default();
        assert_eq!(reserve_width(&ctx), 138.0);
        set_state(
            &ctx,
            WindowState {
                fullscreen: true,
                ..Default::default()
            },
        );
        begin_frame(&ctx);
        header(
            &ctx,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, HEIGHT)),
        );
        assert_eq!(reserve_width(&ctx), 0.0);
        assert!(!geometry(&ctx).drag_enabled);
    }
}
