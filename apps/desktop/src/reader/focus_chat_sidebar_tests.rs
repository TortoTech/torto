use super::*;
use crate::reader::{AssistantPresentation, ChatStreamMessage, ChatStreamingState, Motion};

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

#[test]
fn shortcuts_use_independent_book_and_paragraph_sessions_and_keep_hidden_streams() {
    let mut reader = reader();
    let ctx = egui::Context::default();
    let sidebar = reader.shortcuts.toggle_right_sidebar;
    let popup = reader.shortcuts.focus_chat;
    shortcut(&mut reader, &ctx, sidebar);
    assert!(reader.assistant_is_docked());
    assert!(reader.focus_chat_session_key.is_none());
    let book_session = reader.chat.session_id;
    reader.chat.input = "Book draft".into();
    reader.chat.streaming = Some(ChatStreamingState {
        thinking_seconds: None,
        started: Instant::now(),
        task_id: 77,
        content: "First book answer".into(),
        progress: Vec::new(),
        reasoning_index: None,
        tools: Default::default(),
    });
    shortcut(&mut reader, &ctx, sidebar);
    shortcut(&mut reader, &ctx, popup);
    assert!(reader.focus_assistant_popup_visible());
    assert_ne!(reader.chat.session_id, book_session);
    assert!(reader.chat.input.is_empty());
    assert!(reader.chat.streaming.is_none());
    reader.chat.input = "Paragraph draft".into();
    let paragraph_session = reader.chat.session_id;
    reader.update_chat_stream(ChatStreamMessage {
        id: 77,
        session_id: book_session,
        content: crate::plugins::ChatStreamEvent::Content("Book answer continued".into()),
    });
    assert!(reader.chat.streaming.is_none());
    shortcut(&mut reader, &ctx, sidebar);
    assert_eq!(reader.chat.session_id, book_session);
    assert_eq!(reader.chat.input, "Book draft");
    assert!(reader.chat.references.is_empty());
    assert_eq!(
        reader.chat.streaming.as_ref().unwrap().content,
        "Book answer continued"
    );
    shortcut(&mut reader, &ctx, sidebar);
    assert!(reader.ui.assistant_panel.is_none());
    shortcut(&mut reader, &ctx, popup);
    assert_eq!(reader.chat.session_id, paragraph_session);
    assert_eq!(reader.chat.input, "Paragraph draft");
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
    for (width, expected_sidebar) in [(1_200.0, 560.0), (748.0, 300.0), (1_200.0, 560.0)] {
        let mut remaining = 0.0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 800.0))),
                ..Default::default()
            },
            |ui| {
                reader.show_side_panels(ui);
                remaining = ui.available_rect_before_wrap().width();
            },
        );
        output.shapes.clear();
        output.textures_delta.clear();
        let panel =
            egui::containers::panel::PanelState::load(&ctx, egui::Id::new("reader-assistant"))
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
            data.insert_temp(egui::Id::new("reader-footnote-popup-rect"), footnote_rect)
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
