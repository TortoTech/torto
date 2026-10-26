use super::*;
use std::sync::Arc;

#[test]
fn feedback_focus_snapshot_tracks_the_current_source_unit_and_survives_navigation() {
    let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
    reader.reading_mode = crate::preferences::ReadingMode::Focus;
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    assert!(reader.focus_units.len() >= 2);
    reader.focus_unit_index = 0;
    // Sentence splitting keeps the node but narrows the original scalar offsets.
    reader.focus_units[0].range.start.text_offset = 4;
    reader.focus_units[0].range.end.text_offset = 19;
    reader.focus_units[0].paint_ranges = vec![reader.focus_units[0].range.clone()];
    let ctx = egui::Context::default();
    let captured = reader.feedback_context(&ctx);
    let location = captured.context.location.as_ref().unwrap();
    assert_eq!(location.document.as_deref(), Some("chapter.xhtml"));
    assert!(location.anchor.is_none());
    let focus = location.focus.as_ref().unwrap();
    assert_eq!(
        focus.range.start.node,
        reader.focus_units[0].range.start.node
    );
    assert_eq!(focus.range.start.text_offset, 4);
    assert_eq!(focus.range.end.text_offset, 19);
    reader.prepare_feedback_request();
    reader.focus_unit_index = 1;
    let next = reader.feedback_context(&ctx);
    assert_ne!(
        serde_json::to_value(&captured.context.location).unwrap(),
        serde_json::to_value(&next.context.location).unwrap()
    );
    assert_eq!(focus.range.start.text_offset, 4);
    // The wire contract uses camelCase offsets, without any paragraph text.
    let wire = serde_json::to_value(&captured.context).unwrap();
    assert_eq!(wire["location"]["focus"]["range"]["start"]["textOffset"], 4);
    assert!(wire["location"]["focus"].get("text").is_none());
    let restored: crate::feedback::Location =
        serde_json::from_value(wire["location"].clone()).unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), wire["location"]);
}

#[test]
fn feedback_omits_focus_for_classic_unprepared_and_stale_units() {
    let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
    reader.reading_mode = crate::preferences::ReadingMode::Focus;
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    let ctx = egui::Context::default();
    assert!(
        reader
            .feedback_context(&ctx)
            .context
            .location
            .unwrap()
            .focus
            .is_some()
    );
    reader.reading_mode = crate::preferences::ReadingMode::Classic;
    let legacy = serde_json::to_value(reader.feedback_context(&ctx).context).unwrap();
    assert!(legacy["location"].get("focus").is_none());
    reader.reading_mode = crate::preferences::ReadingMode::Focus;
    reader.focus_units_ready = false;
    assert!(
        reader
            .feedback_context(&ctx)
            .context
            .location
            .unwrap()
            .focus
            .is_none()
    );
    reader.focus_units_ready = true;
    reader.focus_units[reader.focus_unit_index]
        .position
        .section_index = usize::MAX;
    assert!(
        reader
            .feedback_context(&ctx)
            .context
            .location
            .unwrap()
            .focus
            .is_none()
    );
}

#[test]
fn feedback_keeps_members_and_source_ids_in_translation_and_pdf_reflow() {
    let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
    reader.reading_mode = crate::preferences::ReadingMode::Focus;
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    reader.focus_unit_index = 0;
    let second = reader.focus_units[1].range.clone();
    reader.focus_units[0].paint_ranges.push(second.clone());
    reader.focus_units[0].range.end = second.end;
    let ctx = egui::Context::default();
    let expected = serde_json::to_value(
        reader
            .feedback_context(&ctx)
            .context
            .location
            .unwrap()
            .focus,
    )
    .unwrap();
    assert_eq!(expected["totalRanges"], 2);
    assert_eq!(expected["ranges"].as_array().unwrap().len(), 2);
    reader.translation.render_enabled = true;
    assert_eq!(
        serde_json::to_value(
            reader
                .feedback_context(&ctx)
                .context
                .location
                .unwrap()
                .focus
        )
        .unwrap(),
        expected
    );
    reader.format = BookFormat::Pdf;
    reader.pdf_ocr.mode = super::super::PdfOcrViewMode::Reflow;
    assert_eq!(
        serde_json::to_value(
            reader
                .feedback_context(&ctx)
                .context
                .location
                .unwrap()
                .focus
        )
        .unwrap(),
        expected
    );
    let image = reader
        .focus_units
        .iter()
        .position(|unit| unit.is_image)
        .unwrap();
    reader.focus_unit_index = image;
    let image_focus = reader
        .feedback_context(&ctx)
        .context
        .location
        .unwrap()
        .focus
        .unwrap();
    assert_eq!(image_focus.range.start.node, "image");
    assert_eq!(
        image_focus.range.start.text_offset,
        image_focus.range.end.text_offset
    );
}

#[test]
fn unchanged_settings_keep_prepared_pages_focus_units_and_restore_target() {
    let (mut reader, _, range) = crate::reader::semantic_layout::tests::fixture();
    let style = reader.reader.style();
    let mut settings = AppliedSettings {
        spread: style.spread,
        reading_mode: reader.reading_mode,
        focus_style: reader.focus_style,
        hide_cursor_in_focus_mode: reader.hide_cursor_in_focus_mode,
        interface_typography: Default::default(),
        typography: style.typography,
        typesetting: style.typesetting,
        plugin_settings: reader.plugin_settings.clone(),
        language: reader.language,
        theme: Default::default(),
        selection_granularity: reader.selection_granularity,
        shortcuts: reader.shortcuts.clone(),
        sync_settings: reader.sync_settings.clone(),
        sync_password: reader.sync_password.clone(),
    };
    reader.apply_global_settings(&settings);
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    let units = reader.focus_units.len();
    reader.scroll_target_source = Some(range.clone());
    reader.apply_global_settings(&settings);
    assert!(Arc::ptr_eq(
        &layout,
        reader.scroll_section.as_ref().unwrap()
    ));
    assert_eq!(reader.focus_units.len(), units);
    assert!(reader.focus_units_ready);
    assert_eq!(reader.scroll_target_source, Some(range));

    reader.scroll_viewport = Some(crate::reader::ScrollViewportState {
        size: egui::vec2(800.0, 600.0),
        offset_y: 0.0,
    });
    reader.page_scene();
    let cached = reader.page_scenes.clone();
    assert!(!cached.is_empty());
    let revision = reader.scene_revision;
    settings.focus_style = crate::preferences::FocusStyle::SurroundingDim;
    reader.apply_global_settings(&settings);
    assert_eq!(reader.focus_style, settings.focus_style);
    assert_ne!(reader.scene_revision, revision);
    assert!(Arc::ptr_eq(
        &layout,
        reader.scroll_section.as_ref().unwrap()
    ));
    reader.page_scene();
    assert!(
        cached
            .iter()
            .all(|(key, value)| Arc::ptr_eq(value, &reader.page_scenes[key]))
    );
    assert_eq!(reader.focus_units.len(), units);
    let revision = reader.scene_revision;
    reader.apply_global_settings(&settings);
    assert_eq!(
        reader.scene_revision, revision,
        "unchanged focus style is quiet"
    );

    settings.typography.font_size += 1.0;
    reader.apply_global_settings(&settings);
    assert!(reader.scroll_section.is_none());
    assert!(reader.focus_units.is_empty());
}
