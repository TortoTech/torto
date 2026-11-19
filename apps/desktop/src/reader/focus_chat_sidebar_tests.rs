use super::*;
use crate::reader::{AssistantPresentation, ChatStreamMessage, ChatStreamingState, Motion, ScrollViewportState};

fn reader() -> DesktopReader {
    let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
    reader.reading_mode = crate::preferences::ReadingMode::Focus;
    let mut style = reader.reader.style();
    style.minimum_content_width = FOCUS_MIN_TEXT_WIDTH;
    style.minimum_horizontal_margin = FOCUS_MIN_SIDE_SPACE;
    reader.reader.set_style(style).unwrap();
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    reader
}

#[test]
fn table_chart_action_tracks_focused_table_and_shortcut_ignores_repeats() {
    let (_, original, range) = crate::reader::semantic_layout::tests::fixture();
    let mut table = rebook_html::parse_recognized_table(
        "<table><tr><th>Category</th><th>Value</th></tr><tr><td>A</td><td>1</td></tr><tr><td>B</td><td>2</td></tr></table>",
    ).unwrap();
    table.source = Some(range.clone());
    let (mut reader, original, _) = crate::reader::semantic_layout::tests::fixture_with_blocks(
        vec![original.blocks[0].clone(), rebook_publication::Block::Table(table)],
    );
    reader.plugin_settings.semantic_layout.enabled = true;
    reader.semantic_source.configure("chart-toolbar-test", &reader.plugin_settings);
    let recognition = serde_json::from_value(serde_json::json!({
        "fingerprint":crate::plugins::semantic_layout::fingerprint(&original),
        "formulas_checked":true,"annotations":[{"TableChart":{
            "source":range,"members":[range],"language":"English",
            "svg":"<svg xmlns='http://www.w3.org/2000/svg' width='100' height='60'><path d='M0 60L100 0'/></svg>"
        }}]
    })).unwrap();
    assert!(reader.semantic_source.install(0, recognition));
    reader.reader = reader.reader.prepare_refresh_request(None).prepare().unwrap();
    reader.reading_mode = crate::preferences::ReadingMode::Focus;
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    reader.select_focus_unit(0);
    assert!(reader.current_focus_chart_control().is_none());
    reader.select_focus_unit(reader.focus_units.iter().position(|u| u.range.start.node == range.start.node).unwrap());
    assert_eq!(reader.current_focus_chart_control().unwrap().1, false);
    let ctx = egui::Context::default();
    let binding = reader.shortcuts.focus_table_chart;
    let press = |repeat| egui::RawInput {
        events: vec![egui::Event::Key {
            key: binding.logical_key, physical_key: None, pressed: true, repeat,
            modifiers: binding.modifiers,
        }], ..Default::default()
    };
    let mut output = ctx.run_ui(press(false), |_| reader.keyboard_shortcuts(&ctx, false));
    output.textures_delta.clear();
    assert!(reader.current_focus_chart_control().unwrap().1);
    output = ctx.run_ui(press(true), |_| reader.keyboard_shortcuts(&ctx, false));
    output.textures_delta.clear();
    assert!(reader.current_focus_chart_control().unwrap().1);
    output = ctx.run_ui(egui::RawInput {
        events: vec![egui::Event::Key {
            key: binding.logical_key, physical_key: None, pressed: false, repeat: false,
            modifiers: binding.modifiers,
        }], ..Default::default()
    }, |_| {});
    output.textures_delta.clear();
    shortcut(&mut reader, &ctx, binding);
    assert!(!reader.current_focus_chart_control().unwrap().1);
    reader.focus_units_ready = false;
    assert!(reader.current_focus_chart_control().is_none());
}

fn shortcut(reader: &mut DesktopReader, ctx: &egui::Context, binding: egui::KeyboardShortcut) {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_200.0, 800.0))),
            events: vec![egui::Event::Key {
                key: binding.logical_key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: binding.modifiers,
            }],
            ..Default::default()
        },
        |_| reader.keyboard_shortcuts(ctx, false),
    );
    output.shapes.clear();
    output.textures_delta.clear();
    // Allow finite transitions to finish before the next physical key press.
    reader.advance_frame(Instant::now() + Duration::from_millis(200));
    // Every call is a separate physical key press.
    let mut output = ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: binding.logical_key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: binding.modifiers,
            }],
            ..Default::default()
        },
        |_| {},
    );
    output.shapes.clear();
    output.textures_delta.clear();
}

fn sidebar_frame(reader: &mut DesktopReader, ctx: &egui::Context, events: Vec<egui::Event>) {
    reader.advance_frame(Instant::now() + Duration::from_millis(200));
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_200.0, 800.0))),
            events,
            ..Default::default()
        },
        |ui| {
            reader.ui(ui, None, false);
        },
    );
    output.shapes.clear();
    output.textures_delta.clear();
}

fn key_event(key: egui::Key, pressed: bool, repeat: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat,
        modifiers: egui::Modifiers::NONE,
    }
}

/// Focus mode keeps one conversation per paragraph and the classic mode keeps
/// the book's own one, so a draft and a hidden stream survive every switch.
#[test]
fn paragraphs_and_the_book_keep_independent_sessions_and_hidden_streams() {
    let mut reader = reader();
    let ctx = egui::Context::default();
    let panel = reader.shortcuts.focus_chat;
    shortcut(&mut reader, &ctx, panel);
    assert!(reader.focus_assistant_popup_visible());
    assert!(reader.focus_chat_session_key.is_some());
    let first_session = reader.chat.session_id;
    reader.chat.input = "First draft".into();
    reader.chat.streaming = Some(ChatStreamingState {
        thinking_seconds: None,
        started: Instant::now(),
        task_id: 77,
        content: "First answer".into(),
        progress: Vec::new(),
        reasoning_index: None,
        tools: Default::default(),
    });
    // Turning a paragraph swaps in that paragraph's own, empty conversation.
    reader.select_focus_unit(1);
    assert_eq!(reader.focus_unit_index, 1);
    assert_ne!(reader.chat.session_id, first_session);
    assert!(reader.chat.input.is_empty());
    assert!(reader.chat.streaming.is_none());
    reader.chat.input = "Second draft".into();
    let second_session = reader.chat.session_id;
    // A stream that belongs to the first session never lands in this one.
    reader.update_chat_stream(ChatStreamMessage {
        id: 77,
        session_id: first_session,
        content: crate::plugins::ChatStreamEvent::Content("First answer continued".into()),
    });
    assert!(reader.chat.streaming.is_none());
    // Going back restores the first paragraph's draft and its stream.
    reader.select_focus_unit(0);
    assert_eq!(reader.chat.session_id, first_session);
    assert_eq!(reader.chat.input, "First draft");
    assert_eq!(
        reader.chat.streaming.as_ref().unwrap().content,
        "First answer continued"
    );
    // The classic mode shows the book's conversation instead, and the paragraph
    // conversations are still there when focus mode comes back.
    reader.reading_mode = crate::preferences::ReadingMode::Classic;
    reader.restore_book_chat_session();
    assert!(reader.focus_chat_session_key.is_none());
    assert_ne!(reader.chat.session_id, first_session);
    assert_ne!(reader.chat.session_id, second_session);
    reader.reading_mode = crate::preferences::ReadingMode::Focus;
    reader.select_focus_unit(1);
    assert_eq!(reader.chat.session_id, second_session);
    assert_eq!(reader.chat.input, "Second draft");
}

#[test]
fn chat_shortcut_from_body_focuses_open_sidebar_input_and_keeps_book_session() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    reader.chat.input = "Book draft".into();
    let session = reader.chat.session_id;
    let ctx = egui::Context::default();
    let binding = reader.shortcuts.focus_chat;
    for frame in 0..4 {
        reader.advance_frame(Instant::now() + Duration::from_millis(200));
        if frame == 1 {
            reader.ui.assistant_keyboard_focus = false;
            ctx.memory_mut(egui::Memory::stop_text_input);
        }
        let events = match frame {
            1 => vec![egui::Event::Key {
                key: binding.logical_key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: binding.modifiers,
            }],
            2 => vec![
                egui::Event::Key {
                    key: binding.logical_key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: binding.modifiers,
                },
                egui::Event::Text(" continued".into()),
            ],
            _ => Vec::new(),
        };
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_200.0, 800.0))),
                events,
                ..Default::default()
            },
            |ui| {
                reader.ui(ui, None, false);
            },
        );
        output.shapes.clear();
        output.textures_delta.clear();
    }
    assert!(ctx.text_edit_focused());
    assert!(reader.assistant_is_docked());
    assert!(!reader.focus_assistant_popup_visible());
    assert_eq!(reader.chat.session_id, session);
    assert_eq!(reader.chat.input, "Book draft continued");
    assert!(reader.chat.references.is_empty());
    assert!(reader.focus_chat_session_key.is_none());
}

#[test]
fn sidebar_tab_returns_to_body_and_completion_keeps_input_focus() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    reader.chat.input = "Book draft".into();
    let session = reader.chat.session_id;
    let ctx = egui::Context::default();
    sidebar_frame(&mut reader, &ctx, Vec::new());
    sidebar_frame(&mut reader, &ctx, Vec::new());
    assert!(ctx.text_edit_focused());

    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::Tab, true, false)],
    );
    assert!(!ctx.text_edit_focused());
    assert!(reader.focus_body_accepts_shortcuts(false));
    assert_eq!(reader.ui.assistant_panel, Some(AssistantPanel::Chat));
    assert_eq!(reader.chat.input, "Book draft");
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::Tab, false, false)],
    );

    let unit = reader.focus_unit_index;
    assert!(unit + 1 < reader.focus_units.len());
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, true, false)],
    );
    assert_eq!(reader.focus_unit_index, unit + 1);
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, false, false)],
    );
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::Tab, true, false)],
    );
    assert!(ctx.text_edit_focused());
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::Tab, true, true)],
    );
    assert!(ctx.text_edit_focused());
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::Tab, false, false)],
    );

    for token in ["/", "@"] {
        reader.chat.input = token.into();
        reader.chat.move_cursor_to_end = true;
        sidebar_frame(&mut reader, &ctx, Vec::new());
        let (references, commands) = reader.assistant_suggestions(false);
        assert!(active_suggestion_count(&references, &commands) > 0);
        sidebar_frame(
            &mut reader,
            &ctx,
            vec![key_event(egui::Key::Tab, true, false)],
        );
        assert!(ctx.text_edit_focused());
        assert!(reader.ui.assistant_keyboard_focus);
        assert_ne!(reader.chat.input, token);
        sidebar_frame(
            &mut reader,
            &ctx,
            vec![key_event(egui::Key::Tab, false, false)],
        );
    }
    assert!(reader.assistant_is_docked());
    assert_eq!(reader.chat.session_id, session);
    assert!(reader.focus_chat_session_key.is_none());
}

#[test]
fn navigating_with_sidebar_open_keeps_the_book_conversation() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    let session = reader.chat.session_id;
    reader.chat.input = "Keep discussing this book".into();
    reader.ui.assistant_keyboard_focus = false;
    assert!(reader.focus_body_accepts_shortcuts(false));
    assert!(reader.focus_units.len() > 1);
    reader.select_focus_unit(1);
    assert_eq!(reader.chat.session_id, session);
    assert_eq!(reader.chat.input, "Keep discussing this book");
    assert_eq!(reader.ui.assistant_panel, Some(AssistantPanel::Chat));
    assert_eq!(reader.ui.assistant_motion.target, 1.0);
    assert!(reader.focus_chat_session_key.is_none());
}

#[test]
fn sidebar_resize_and_close_keep_the_selected_source_anchor() {
    let mut reader = reader();
    reader.select_focus_unit(1);
    let anchor = reader.focus_units[reader.focus_unit_index]
        .range
        .start
        .clone();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    let session = reader.chat.session_id;
    let ctx = egui::Context::default();
    for (width, preferred, open) in [
        (1_200.0, 340.0, true),
        (1_200.0, 560.0, true),
        (748.0, 560.0, true),
        (1_200.0, 560.0, false),
    ] {
        reader.ui.assistant_width = preferred;
        if !open {
            reader.close_assistant_panel();
        }
        for _ in 0..3 {
            reader.advance_frame(Instant::now() + Duration::from_millis(200));
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 800.0))),
                    ..Default::default()
                },
                |ui| {
                    reader.ui(ui, None, false);
                },
            );
            output.shapes.clear();
            output.textures_delta.clear();
        }
        assert_eq!(
            reader.focus_units[reader.focus_unit_index].range.start,
            anchor
        );
        assert_eq!(reader.chat.session_id, session);
        assert_eq!(reader.ui.assistant_motion.is_animating(), false);
    }
}

#[test]
fn sidebar_animation_reserves_width_once_and_releases_it_after_closing() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    let ctx = egui::Context::default();
    let mut widths = Vec::new();
    for progress in [0.0, 0.25, 0.75, 1.0, 0.75, 0.25] {
        reader.ui.assistant_motion.value = progress;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                reader.show_side_panels(ui);
                widths.push(ui.available_width());
            },
        );
        output.textures_delta.clear();
    }
    assert!(
        widths
            .windows(2)
            .all(|pair| (pair[0] - pair[1]).abs() < 0.1),
        "{widths:?}"
    );
    reader.close_assistant_panel();
    assert!(reader.ui.assistant_panel.is_some());
    reader.advance_motion(Instant::now() + Duration::from_millis(200));
    assert!(reader.ui.assistant_panel.is_none());
    assert!(!reader.ui.assistant_motion.is_animating());
    let mut restored = 0.0;
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            reader.show_side_panels(ui);
            restored = ui.available_width();
        },
    );
    output.textures_delta.clear();
    assert!(restored > widths[0] + 250.0);
}

#[test]
fn assistant_sidebar_moves_and_fades_background_input_and_actions_together() {
    #[derive(Debug)]
    struct Sample { background: Rect, input: Rect, action: Rect }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for mode in [crate::preferences::ReadingMode::Focus, crate::preferences::ReadingMode::Classic] {
        for (width, scale) in [(900.0, 1.0), (1441.0, 2.0)] {
            let mut reader = reader();
            reader.reading_mode = mode;
            reader.language = crate::preferences::AppLanguage::English;
            reader.toggle_assistant_panel(AssistantPanel::Chat);
            let ctx = egui::Context::default();
            crate::ui::configure(&ctx, &Default::default(), rebook_layout::default_monospace_font(),
                crate::preferences::AppLanguage::English, runtime.handle());
            ctx.set_pixels_per_point(scale);
            let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 901.0));
            let mut sample = |progress: f32, closing: bool| {
                reader.ui.assistant_motion.value = progress;
                reader.ui.assistant_motion.target = if closing { 0.0 } else { 1.0 };
                let mut result = None;
                for pass in 0..2 {
                    let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() },
                        |ui| { reader.show_side_panels(ui); });
                    if pass == 1 {
                        let mut background = None;
                        let mut input = None;
                        let mut action = None;
                        let alpha = (255.0 * progress).round() as u8;
                        for shape in &output.shapes {
                            if let egui::Shape::Rect(rect) = &shape.shape {
                                if rect.fill.a() > 0 && (rect.rect.height() - screen.height()).abs() < 1.0 {
                                    assert_eq!(rect.fill.a(), alpha, "The panel background shares the content fade");
                                    background = Some(rect.rect);
                                }
                                if rect.corner_radius.nw == 9 && rect.rect.top() > 500.0 {
                                    assert_eq!(rect.fill.a(), alpha, "Closing must not add disabled dimming");
                                    assert!(shape.clip_rect.contains_rect(rect.rect), "The input must not slide in through its own inset clip");
                                    input = Some(rect.rect);
                                }
                                if rect.brush.is_some() && rect.rect.top() > TOOLBAR_HEIGHT && rect.rect.bottom() < TOOLBAR_HEIGHT + 60.0 {
                                    assert_eq!(rect.fill.a(), alpha);
                                    assert!(shape.clip_rect.contains_rect(rect.rect), "New conversation must be visible throughout the fade");
                                    action = Some(rect.rect);
                                }
                            }
                        }
                        result = Some(Sample { background: background.expect("moving panel background"),
                            input: input.expect("composer"), action: action.expect("new conversation cannot appear late") });
                    }
                    output.shapes.clear();
                    output.textures_delta.clear();
                }
                result.unwrap()
            };
            let settled = sample(1.0, false);
            for closing in [false, true] {
                for progress in [0.1, 0.25, 0.5, 0.75, 0.9, 1.0, 0.75, 0.25] {
                    let current = sample(progress, closing);
                    let shift = current.background.left() - settled.background.left();
                    if progress < 1.0 { assert!(shift > 0.0, "Keep the slide as well as the fade"); }
                    assert!((current.input.left() - settled.input.left() - shift).abs() < 0.6, "{current:?}");
                    assert!((current.action.left() - settled.action.left() - shift).abs() < 0.6, "{current:?}");
                }
            }
            let point = sample(0.5, true).action.center();
            drop(sample);
            reader.chat.input = "Keep this draft while closing".into();
            reader.ui.assistant_keyboard_focus = false;
            let session = reader.chat.session_id;
            for pressed in [true, false] {
                let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen),
                    events: vec![egui::Event::PointerMoved(point), egui::Event::PointerButton {
                        pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    }], ..Default::default() }, |ui| { reader.show_side_panels(ui); });
                output.shapes.clear();
                output.textures_delta.clear();
            }
            assert_eq!(reader.chat.session_id, session, "Closing controls must remain non-interactive");
            assert_eq!(reader.chat.input, "Keep this draft while closing");
            assert!(!reader.ui.assistant_keyboard_focus);
        }
    }
}

#[test]
fn directory_drawer_dims_assistant_divider_and_blocks_its_drag() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    reader.ui.sidebar_pinned = false;
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_200.0, 800.0));
    let edge = screen.right() - reader.ui.assistant_width;
    for progress in [0.0, 0.25, 1.0, 0.5, 0.0] {
        for frame in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events: vec![egui::Event::PointerMoved(Pos2::new(edge, 200.0))],
                    ..Default::default()
                },
                |ui| {
                    reader.show_side_panels(ui);
                    if progress > 0.0 {
                        reader.floating_sidebar(&ctx, progress);
                    }
                    reader.resize_side_panels(&ctx, progress, 1.0, false);
                },
            );
            output.textures_delta.clear();
            if frame == 0 {
                continue;
            }
            let divider = output
                .shapes
                .iter()
                .position(|shape| {
                    matches!(&shape.shape, egui::Shape::LineSegment { points, .. }
                    if (points[0].x - edge).abs() < 0.1
                        && points[0].y < 50.0 && points[1].y == screen.bottom())
                })
                .expect("assistant divider is painted");
            if progress > 0.0 {
                let scrim = output
                    .shapes
                    .iter()
                    .position(|shape| {
                        matches!(&shape.shape, egui::Shape::Rect(rect)
                        if rect.rect == screen
                            && rect.fill == Color32::BLACK.gamma_multiply(0.31 * progress))
                    })
                    .expect("directory backdrop is painted");
                assert!(
                    divider < scrim,
                    "divider must be underneath the directory backdrop"
                );
                assert_ne!(
                    output.platform_output.cursor_icon,
                    egui::CursorIcon::ResizeHorizontal
                );
            }
        }
    }
    let original_width = reader.ui.assistant_width;
    for (point, pressed) in [
        (Pos2::new(edge, 200.0), Some(true)),
        (Pos2::new(700.0, 200.0), None),
        (Pos2::new(700.0, 200.0), Some(false)),
    ] {
        let mut events = vec![egui::Event::PointerMoved(point)];
        if let Some(pressed) = pressed {
            events.push(egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            },
            |ui| {
                reader.show_side_panels(ui);
                reader.floating_sidebar(&ctx, 1.0);
                reader.resize_side_panels(&ctx, 1.0, 1.0, false);
            },
        );
        output.textures_delta.clear();
        assert_eq!(reader.ui.assistant_width, original_width);
    }
}

#[test]
fn sidebar_divider_drag_is_clamped_to_leave_room_for_text() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    let ctx = egui::Context::default();
    for frame in 0..8 {
        let point = if frame < 6 {
            Pos2::new(860.0, 200.0)
        } else {
            Pos2::new(450.0, 200.0)
        };
        let mut events = vec![egui::Event::PointerMoved(point)];
        if frame == 5 || frame == 7 {
            events.push(egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: frame == 5,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_200.0, 800.0))),
                time: Some(f64::from(frame) * 0.2),
                events,
                ..Default::default()
            },
            |ui| {
                reader.show_side_panels(ui);
                reader.resize_side_panels(&ctx, 0.0, 1.0, false);
            },
        );
        output.shapes.clear();
        output.textures_delta.clear();
    }
    assert_eq!(reader.ui.assistant_width, ASSISTANT_MAX_WIDTH);
    assert!(1_200.0 - reader.ui.assistant_width >= FOCUS_MIN_READER_WIDTH);
}

#[test]
fn keyboard_navigation_belongs_to_the_active_area() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    reader.chat.messages.push(crate::plugins::ChatTurn {
        thinking_seconds: None,
        progress: Vec::new(),
        images: Vec::new(),
        role: ChatRole::Assistant,
        content: "Book answer".into(),
        display_content: None,
    });
    let ctx = egui::Context::default();
    let next = reader.shortcuts.next_page_or_paragraph;
    let initial = reader.focus_unit_index;
    shortcut(&mut reader, &ctx, next);
    assert_eq!(reader.focus_unit_index, initial);
    assert!(reader.chat.pending_keyboard_scroll_delta > 0.0);
    reader.chat.pending_keyboard_scroll_delta = 0.0;
    reader.ui.assistant_keyboard_focus = false;
    shortcut(&mut reader, &ctx, next);
    assert_ne!(reader.focus_unit_index, initial);
    assert_eq!(reader.chat.pending_keyboard_scroll_delta, 0.0);
    assert_eq!(reader.ui.assistant_panel, Some(AssistantPanel::Chat));
}

#[test]
fn escape_dismisses_completion_before_closing_without_erasing_the_draft() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    reader.chat.input = "/".into();
    reader.chat.cursor_char_index = 1;
    let (references, commands) = reader.assistant_suggestions(false);
    assert!(active_suggestion_count(&references, &commands) > 0);
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        ui.text_edit_singleline(&mut reader.chat.input)
            .request_focus();
    });
    output.textures_delta.clear();
    assert!(ctx.text_edit_focused());
    let escape = egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Escape);
    shortcut(&mut reader, &ctx, escape);
    assert!(reader.ui.assistant_panel.is_some());
    assert!(reader.chat.suggestions_dismissed);
    assert_eq!(reader.chat.input, "/");
    shortcut(&mut reader, &ctx, escape);
    assert!(reader.ui.assistant_panel.is_none());
    assert_eq!(reader.chat.input, "/");
}

#[test]
fn sidebar_reserves_text_width_and_margins_and_remembers_preferred_width() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    reader.ui.assistant_width = 560.0;
    let ctx = egui::Context::default();
    let mut panel_id = egui::Id::NULL;
    for (width, expected_sidebar) in [(1_200.0, 560.0), (748.0, 300.0), (1_200.0, 560.0)] {
        let mut remaining = 0.0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 800.0))),
                ..Default::default()
            },
            |ui| {
                panel_id = ui.make_persistent_id("reader-assistant");
                reader.show_side_panels(ui);
                remaining = ui.available_rect_before_wrap().width();
            },
        );
        output.shapes.clear();
        output.textures_delta.clear();
        let panel =
            egui::containers::panel::PanelState::load(&ctx, panel_id)
                .unwrap();
        assert!(
            (panel.size().x - expected_sidebar).abs() < 1.0,
            "panel={panel:?}"
        );
        assert!(remaining >= FOCUS_MIN_READER_WIDTH);
        let style = reader.reader.style();
        assert!(reading_content_width(remaining, &style) >= 399.0);
        assert!(reading_content_left(remaining, &style) >= FOCUS_MIN_SIDE_SPACE);
        assert_eq!(reader.ui.assistant_width, 560.0);
    }
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(747.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            panel_id = ui.make_persistent_id("reader-assistant");
                reader.show_side_panels(ui);
        },
    );
    output.shapes.clear();
    output.textures_delta.clear();
    assert!(reader.ui.assistant_panel.is_none());
    assert_eq!(reader.ui.assistant_width, 560.0);
    assert_eq!(
        reader.ui.assistant_presentation,
        AssistantPresentation::Sidebar
    );
}

#[test]
fn footnote_popup_can_cover_sidebar_and_stays_inside_the_window() {
    for width in [748.0, 800.0, 1_000.0, 1_400.0] {
        let sidebar_width = focus_assistant_sidebar_width(width, 340.0).unwrap();
        let page_right = width - sidebar_width;
        let viewport = Rect::from_min_max(Pos2::new(0.0, 40.0), Pos2::new(width, 800.0));
        let (x, minimum, maximum) =
            footnote_popup_sidebar_bounds(page_right - 24.0, viewport).unwrap();
        assert!(minimum >= 240.0);
        assert!(x + maximum > page_right);
        assert!(x >= viewport.left() + 16.0);
        assert!(x + maximum <= viewport.right() - 16.0);
    }
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 600.0));
    let (x, minimum, maximum) = footnote_popup_sidebar_bounds(480.0, viewport).unwrap();
    assert_eq!(minimum, 240.0);
    assert!(x < 480.0);
    assert!(x + maximum <= 484.0);
}

#[test]
fn sidebar_wheel_does_not_steal_body_or_footnote_scroll() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    reader.ui.assistant_motion = Motion::settled(1.0);
    let ctx = egui::Context::default();
    let footnote_rect = Rect::from_min_max(Pos2::new(850.0, 150.0), Pos2::new(1_150.0, 400.0));
    for (point, footnotes, should_route) in [
        (Pos2::new(200.0, 300.0), false, false),
        (Pos2::new(1_000.0, 300.0), false, true),
        (Pos2::new(1_000.0, 300.0), true, false),
        (Pos2::new(1_000.0, 500.0), true, true),
    ] {
        reader.ui.focus_footnotes_visible = footnotes;
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::unique("reader-footnote-popup-rect"), footnote_rect)
        });
        let mut routed = 0.0;
        let mut untouched = 0.0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_200.0, 800.0))),
                events: vec![egui::Event::PointerMoved(point)],
                ..Default::default()
            },
            |ui| {
                ui.ctx()
                    .input_mut(|input| input.smooth_scroll_delta.y = -60.0);
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(Rect::from_min_max(
                        Pos2::new(860.0, 40.0),
                        Pos2::new(1_200.0, 800.0),
                    )),
                    |ui| {
                        routed = reader.assistant_conversation_scroll_delta(ui);
                    },
                );
                untouched = ui.input(|input| input.smooth_scroll_delta.y);
            },
        );
        output.shapes.clear();
        output.textures_delta.clear();
        assert_eq!(routed, if should_route { -60.0 } else { 0.0 });
        assert_eq!(untouched, if should_route { 0.0 } else { -60.0 });
    }
}

/// A closed panel and an empty panel both leave the arrows to the reader; a
/// session with messages takes them from the moment it has one.
#[test]
fn arrows_follow_the_panel_state_and_its_messages() {
    for panel_open in [false, true] {
        for has_messages in [false, true] {
            let mut reader = reader();
            reader.ui.assistant_panel = panel_open.then_some(AssistantPanel::Chat);
            if has_messages {
                reader.chat.messages.push(crate::plugins::ChatTurn {
                    thinking_seconds: None,
                    progress: Vec::new(),
                    images: Vec::new(),
                    role: ChatRole::Assistant,
                    content: "An answer.".into(),
                    display_content: None,
                });
            }
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(0.0),
                    events: vec![egui::Event::Key {
                        key: egui::Key::ArrowDown,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |_| reader.keyboard_shortcuts(&ctx, false),
            );
            output.textures_delta.clear();
            assert_eq!(
                reader.chat.pending_keyboard_scroll_delta != 0.0,
                panel_open && has_messages,
                "panel_open={panel_open}, has_messages={has_messages}"
            );
        }
    }
}

/// Handing the keyboard back to the page returns the arrows to the reader, and
/// an empty conversation does not take them away again.
#[test]
fn arrows_return_to_the_reader_when_the_composer_loses_the_keyboard() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    let ctx = egui::Context::default();
    sidebar_frame(&mut reader, &ctx, Vec::new());
    sidebar_frame(&mut reader, &ctx, Vec::new());
    assert!(ctx.text_edit_focused());
    let unit = reader.focus_unit_index;
    // An empty conversation keeps the arrows in the composer.
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, true, false)],
    );
    assert_eq!(reader.focus_unit_index, unit);
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, false, false)],
    );
    ctx.memory_mut(egui::Memory::stop_text_input);
    sidebar_frame(&mut reader, &ctx, Vec::new());
    assert!(!ctx.text_edit_focused());
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, true, false)],
    );
    assert_eq!(reader.focus_unit_index, unit + 1);
}

/// A conversation with messages keeps the arrows even while the composer holds
/// the keyboard.
#[test]
fn a_conversation_with_messages_keeps_the_arrows_with_the_keyboard() {
    let mut reader = reader();
    reader.toggle_assistant_panel(AssistantPanel::Chat);
    reader.chat.messages.push(crate::plugins::ChatTurn {
        thinking_seconds: None,
        progress: Vec::new(),
        images: Vec::new(),
        role: ChatRole::Assistant,
        content: "Paragraph answer".into(),
        display_content: None,
    });
    let ctx = egui::Context::default();
    sidebar_frame(&mut reader, &ctx, Vec::new());
    sidebar_frame(&mut reader, &ctx, Vec::new());
    assert!(ctx.text_edit_focused());
    let unit = reader.focus_unit_index;
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, true, false)],
    );
    assert_eq!(reader.focus_unit_index, unit);
    sidebar_frame(
        &mut reader,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, false, false)],
    );
}


fn classic_reader() -> DesktopReader {
    let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
    reader.reading_mode = crate::preferences::ReadingMode::Classic;
    let mut style = reader.reader.style();
    style.spread = rebook_layout::SpreadMode::Scroll;
    reader.reader.set_style(style).unwrap();
    reader.scroll_viewport = Some(ScrollViewportState {
        size: egui::vec2(800.0, 800.0),
        offset_y: 0.0,
    });
    reader
}

/// The classic mode summons the same chat panel with Tab, docked like the
/// focus-mode panel. Opening it leaves the conversation context untouched:
/// only a paragraph click attaches one.
#[test]
fn classic_tab_opens_the_chat_panel_without_touching_the_context() {
    let mut reader = classic_reader();
    let ctx = egui::Context::default();
    let binding = reader.shortcuts.focus_chat;
    shortcut(&mut reader, &ctx, binding);
    assert!(reader.assistant_panel_is_open());
    assert_eq!(reader.ui.assistant_panel, Some(AssistantPanel::Chat));
    assert!(reader.assistant_is_docked());
    assert!(
        reader.chat.references.is_empty(),
        "{:?}",
        reader.chat.references
    );
    // Another Tab hands the keyboard to the composer instead of closing.
    shortcut(&mut reader, &ctx, binding);
    assert!(reader.assistant_panel_is_open());
}

/// Clicking a paragraph with the panel open hands that paragraph to the
/// conversation; another click replaces it, and a closed panel keeps the
/// context as it is.
#[test]
fn classic_click_attaches_the_paragraph_under_the_pointer() {
    let mut reader = classic_reader();
    let _layout = reader.current_scroll_layout().unwrap();
    let ctx = egui::Context::default();
    let binding = reader.shortcuts.focus_chat;
    shortcut(&mut reader, &ctx, binding);
    assert!(reader.assistant_panel_is_open());
    let point = (0u32..64)
        .find_map(|row| {
            let y = 24.0 + row as f32 * 12.0;
            (1u32..64).find_map(|column| {
                let x = 40.0 + column as f32 * 12.0;
                reader
                    .hit_test_canvas(x, y, true)
                    .ok()
                    .flatten()
                    .map(|_| (x, y))
            })
        })
        .expect("the fixture page paints text to click");
    reader.attach_clicked_paragraph_reference(point.0, point.1);
    assert_eq!(
        reader.chat.references.len(),
        1,
        "{:?}",
        reader.chat.references
    );
    let reference = &reader.chat.references[0];
    assert!(reference.id.starts_with("focus:"), "{}", reference.id);
    assert!(!reference.label.is_empty());
    assert!(reference.excerpt.is_some());
    // Another click replaces the context instead of stacking references.
    reader.attach_clicked_paragraph_reference(point.0, point.1);
    assert_eq!(reader.chat.references.len(), 1);
    // A closed panel leaves the context alone.
    reader.close_assistant_panel();
    assert!(!reader.assistant_panel_is_open());
    reader.attach_clicked_paragraph_reference(point.0, point.1);
    assert_eq!(reader.chat.references.len(), 1);
}
