use super::*;
use rebook_publication::{
    Book, Metadata, PublicationError, PublicationId, Resource, SpineItem, SpineItemId,
};
use std::sync::atomic::AtomicUsize;

fn reference_reader(body: &str) -> super::super::DesktopReader {
    let (base, _, _) = crate::reader::semantic_layout::tests::fixture();
    let html = format!("<html><body><p>Intro</p>{body}<figure><div><p>First example.</p></div><figcaption>Figure 1 Example</figcaption></figure><table><caption>Table 2 Results</caption><tr><td>Data</td></tr></table><figure><div><p>Third example.</p></div><figcaption>Figure 3 Example</figcaption></figure><table><caption>Table 1 Other table</caption><tr><td>Other</td></tr></table></body></html>");
    let section = rebook_html::parse_section(&html, &base.source.book().sections[0], |_| None).unwrap();
    let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture_with_blocks(section.blocks.clone());
    // The layout fixture accepts blocks only; retain the parsed chapter's anchors
    // in the original source used by the validated reference catalog.
    let original = Arc::new(Source { book: reader.source.book().clone(), sections: vec![section], resources: AtomicUsize::new(0), parses: AtomicUsize::new(0) });
    let links = Arc::new(super::super::media_links::Source::new(reader.rewrite_source.clone(), original));
    links.prepare_for_test();
    reader.media_preview.links = Some(links);
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    let index = reader.focus_units.iter().position(|u| u.text.contains("See")).unwrap();
    reader.select_focus_unit(index);
    reader.ui.sidebar_open = false;
    let (tx, _) = mpsc::sync_channel(1);
    reader.media_preview.worker = Some(Worker { tx, pending: Default::default(), result: Default::default(), generation: Arc::new(AtomicU64::new(0)) });
    reader
}

fn preview_key(reader: &mut super::super::DesktopReader, ctx: &egui::Context, key: egui::Key) {
    let mut output = ctx.run_ui(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0,700.0))),
        events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
        ..Default::default()
    }, |_| {
        if reader.media_preview.active.is_some() { assert!(reader.media_preview_keyboard(ctx)); }
        else { assert!(reader.focus_image_preview_shortcut(ctx, false)); }
    });
    output.textures_delta.clear();
}

#[test]
fn focus_reference_shortcut_orders_deduplicates_and_keeps_navigation_inside_the_modal() {
    let mut reader = reference_reader("<p>See Table 2, Figure 1, Table 2, Figure 99 and Figure 3.</p>");
    let ctx = egui::Context::default();
    let focused = reader.focus_unit_index;
    preview_key(&mut reader, &ctx, egui::Key::Num0);
    let active = reader.media_preview.active.as_ref().unwrap();
    assert_eq!(active.items.iter().map(|i| (i.key.kind,i.key.number.as_str())).collect::<Vec<_>>(),
        [(MediaKind::Table,"2"),(MediaKind::Figure,"1"),(MediaKind::Figure,"3")]);
    preview_key(&mut reader, &ctx, egui::Key::ArrowLeft);
    assert_eq!(reader.media_preview.active.as_ref().unwrap().item_index, 0);
    preview_key(&mut reader, &ctx, egui::Key::ArrowRight);
    preview_key(&mut reader, &ctx, egui::Key::ArrowRight);
    assert_eq!(reader.media_preview.active.as_ref().unwrap().item_index, 2, "rapid navigation works during loading");
    preview_key(&mut reader, &ctx, egui::Key::ArrowRight);
    assert_eq!(reader.media_preview.active.as_ref().unwrap().item_index, 2, "do not wrap");
    assert_eq!(reader.focus_unit_index, focused);
    let worker = reader.media_preview.worker.as_ref().unwrap();
    let pending = worker.pending.load_full().unwrap();
    assert_eq!(pending.key.number, "3");
    let stale_generation = pending.generation;
    let ready = || Ready {
        image: egui::ColorImage::filled([10,10],egui::Color32::WHITE), size:[10,10], captions:vec!["Third".into()], caption_images:vec![],
        previous:None, next:Some(Cursor {chunk:1,page:0}), position:"1 / 2 · 1 / 1".into(),
    };
    worker.result.store(Some(Arc::new(Completion { generation:stale_generation, revision:reader.source.content_revision(),result:Ok(ready()) })));
    reader.accept_media_preview_completion(&ctx);
    let old_texture = reader.media_preview.active.as_ref().unwrap().texture.as_ref().unwrap().id();
    preview_key(&mut reader, &ctx, egui::Key::PageDown);
    let active = reader.media_preview.active.as_ref().unwrap();
    assert_eq!(active.item_index,2);
    assert_eq!(active.texture.as_ref().unwrap().id(),old_texture,"keep the displayed image until the replacement is ready");
    assert_eq!(reader.media_preview.worker.as_ref().unwrap().pending.load_full().unwrap().cursor.chunk,1);
    reader.media_preview.worker.as_ref().unwrap().result.store(Some(Arc::new(Completion { generation:stale_generation, revision:reader.source.content_revision(),result:Ok(ready()) })));
    reader.accept_media_preview_completion(&ctx);
    assert!(reader.media_preview.active.as_ref().unwrap().loading,"stale pixels cannot complete the new request");
    preview_key(&mut reader, &ctx, egui::Key::Escape);
    assert!(reader.media_preview.active.as_ref().unwrap().motion.is_closing());
    assert_eq!(reader.focus_unit_index,focused);
}

#[test]
fn list_reference_previews_cover_the_whole_group_and_clicks_open_the_matching_item() {
    let mut reader = reference_reader("<ol><li><p>See Table 2.</p></li><li><p>See Figure 1.</p></li><li><p>See Figure 3.</p></li><li><p>Again Table 2.</p></li></ol>");
    let unit = &reader.focus_units[reader.focus_unit_index];
    let origin = unit.range.clone();
    let items = reader.focus_reference_previews(&origin);
    assert_eq!(items.iter().map(|i| i.key.number.as_str()).collect::<Vec<_>>(),["2","1","3"]);
    let clicked = items[1].clone();
    reader.open_media_preview(&egui::Context::default(), clicked.origin, clicked.key);
    let active = reader.media_preview.active.as_ref().unwrap();
    assert_eq!(active.item_index,1);
    assert_eq!(active.items.len(),3);
    let sections = reader.reader.current_reading_unit_content().unwrap();
    let repeated = sections.iter().flat_map(|(_,s)| &s.blocks).flat_map(Block::text_blocks).find(|t| plain(t).contains("Again")).unwrap();
    let key = media_references::references("Table 2")[0].key.clone();
    reader.open_media_preview(&egui::Context::default(), repeated.source.clone().unwrap(), key);
    let active = reader.media_preview.active.as_ref().unwrap();
    assert_eq!(active.item_index,0);
    assert_eq!(active.items.len(),3,"clicking a duplicate keeps the complete reference group");
}

#[test]
fn translated_reference_previews_follow_display_order_and_ignore_invented_references() {
    let mut reader = reference_reader("<p>See Figure 1 and Table 2.</p>");
    reader.translation_source.set_mode(crate::plugins::TranslationMode::Replace).unwrap();
    reader.translation_source.set_enabled(true).unwrap();
    reader.translation_source.store_batch(0,&[crate::plugins::BlockTranslation {
        block_index:1, segment_index:None, text:"参见表 2，再看图 1。另外提到表一。".into()
    }]).unwrap();
    reader.snapshot = reader.reader.refresh_source().unwrap();
    reader.scroll_section = None;
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    let unit = reader.focus_units.iter().find(|u|u.text.contains("参见")).unwrap();
    let items = reader.focus_reference_previews(&unit.range);
    assert_eq!(items.iter().map(|i|(i.key.kind,i.key.number.as_str())).collect::<Vec<_>>(),[(MediaKind::Table,"2"),(MediaKind::Figure,"1")]);
}

#[test]
fn reference_navigation_arrows_work_while_loading_and_do_not_dismiss_the_popup() {
    let mut reader = reference_reader("<p>See Table 2, Figure 1 and Figure 3.</p>");
    let ctx = egui::Context::default();
    assert!(reader.open_focus_reference_preview(&ctx));
    let frame = |reader: &mut super::super::DesktopReader, events| {
        let mut output = ctx.run_ui(egui::RawInput {
            time:Some(1.0), screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(900.0,700.0))),
            events, ..Default::default()
        }, |_| reader.media_preview_overlay(&ctx));
        output.textures_delta.clear();
    };
    frame(&mut reader,vec![]);
    frame(&mut reader,vec![]); // Area's initial sizing pass is not interactive.
    let screen = crate::ui::overlay_rect(&ctx);
    let at = egui::pos2(screen.right() - 28.0, screen.center().y);
    frame(&mut reader,vec![egui::Event::PointerMoved(at), egui::Event::PointerButton {pos:at,button:egui::PointerButton::Primary,pressed:true,modifiers:egui::Modifiers::NONE}]);
    frame(&mut reader,vec![egui::Event::PointerButton {pos:at,button:egui::PointerButton::Primary,pressed:false,modifiers:egui::Modifiers::NONE}]);
    let active = reader.media_preview.active.as_ref().unwrap();
    assert_eq!(active.item_index,1);
    assert!(!active.motion.is_closing());
}

#[test]
fn adjacent_subfigures_sharing_a_page_link_remain_separate_preview_items() {
    let mut reader = reference_reader("<ul><li><p>See <a href='#page-103'>Figure 7.2a</a> and <a href='#page-103'>Figure 7.2b</a>.</p></li></ul><figure id='page-103'><figcaption><p>Figure 7.2 Comparison</p><p>(a) First horse.</p></figcaption><img src='first.png'/></figure><figure><figcaption><p>(b) Second horse.</p></figcaption><img src='second.png'/></figure>");
    let sections = reader.reader.current_reading_unit_content().unwrap();
    let text = sections.iter().flat_map(|(_,section)| &section.blocks).flat_map(Block::text_blocks).find(|text| plain(text).contains("See")).unwrap();
    let candidates = reader.media_preview.links.as_ref().unwrap().preview_references(text.source.as_ref().unwrap(), &text.content);
    assert_eq!(candidates.len(), 2, "both shared-anchor references must be valid before opening the modal: {candidates:?}");
    let ctx = egui::Context::default();
    preview_key(&mut reader, &ctx, egui::Key::Num0);
    let active = reader.media_preview.active.as_ref().unwrap();
    assert_eq!(active.items.iter().map(|item| item.key.number.as_str()).collect::<Vec<_>>(), ["7.2a", "7.2b"]);
    assert_ne!(active.items[0].destination, active.items[1].destination);
    let first = reader.media_preview.worker.as_ref().unwrap().pending.load_full().unwrap();
    let a = Index::default().resolve(&first, &|| true).unwrap();
    preview_key(&mut reader, &ctx, egui::Key::ArrowRight);
    let second = reader.media_preview.worker.as_ref().unwrap().pending.load_full().unwrap();
    let b = Index::default().resolve(&second, &|| true).unwrap();
    assert_ne!(a.block, b.block);
    let section = second.source.parse_section(b.section).unwrap();
    let Block::Figure(figure) = &section.blocks[b.block] else { panic!("second figure") };
    assert!(figure.images[0].href.path().ends_with("second.png"));
    assert_eq!(b.group, Some(a.block));
}

#[test]
fn subfigure_inference_stops_at_prose_and_does_not_split_a_combined_figure() {
    let source = source(&["<html><body><figure><figcaption><p>Figure 7.2 Comparison</p><p>(a) First.</p></figcaption><img src='a.png'/></figure><p>Ordinary prose interrupts the group.</p><figure><figcaption>(b) Unrelated.</figcaption><img src='b.png'/></figure><figure><figcaption><p>Figure 8 Combined</p><p>(a) First.</p><p>(b) Second.</p></figcaption><img src='combined.png'/></figure></body></html>"]);
    let targets = targets(&source.sections[0], 0);
    assert!(targets.iter().any(|target| target.key.number == "7.2a"));
    assert!(!targets.iter().any(|target| target.key.number == "7.2b" || target.key.number == "8a" || target.key.number == "8b"));
    assert_eq!(source.resources.load(Ordering::Relaxed), 0);
}

#[test]
#[ignore = "requires original BOOKS without barriers chapter-7.xhtml via TORTO_MEDIA_CHAPTER"]
fn local_book_separate_subfigures_have_distinct_reference_destinations() {
    let html = std::fs::read_to_string(std::env::var("TORTO_MEDIA_CHAPTER").unwrap()).unwrap().replace("chapter-7.xhtml", "s0.xhtml");
    let original = source(&[&html]);
    let catalog = super::super::media_links::Source::new(original.clone(), original.clone());
    catalog.prepare_for_test();
    let text = original.sections[0].blocks.iter().flat_map(Block::text_blocks).find(|text| plain(text).contains("Figure 7.2a")).unwrap();
    let candidates = catalog.preview_references(text.source.as_ref().unwrap(), &text.content);
    assert_eq!(candidates.iter().map(|(key,_,_)| key.number.as_str()).collect::<Vec<_>>(), ["7.2a", "7.2b"]);
    assert_ne!(candidates[0].1.block, candidates[1].1.block);
    for (key, target, _) in candidates {
        let Block::Figure(figure) = &original.sections[0].blocks[target.block] else { panic!("figure") };
        println!("{} -> {}", key.number, figure.images[0].href.path());
    }
    assert_eq!(original.resources.load(Ordering::Relaxed), 0);
}

#[test]
fn focus_shortcut_previews_a_figure_reached_through_a_document_location() {
    let mut reader = reference_reader("<p>See <a href='#position'>Figure 8.2</a>.</p><p id='position'>Ordinary prose before the picture.</p><figure id='border'><img src='border.png'/><figcaption>Figure 8.2 Decorative border</figcaption></figure>");
    let ctx = egui::Context::default();
    preview_key(&mut reader, &ctx, egui::Key::Num0);
    let active = reader.media_preview.active.as_ref().unwrap();
    assert_eq!(active.items.len(), 1);
    assert_eq!(active.key.number, "8.2");
    let pending = reader.media_preview.worker.as_ref().unwrap().pending.load_full().unwrap();
    assert!(pending.destination.is_some());
    let target = Index::default().resolve(&pending, &|| true).unwrap();
    let section = pending.source.parse_section(target.section).unwrap();
    let Block::Figure(figure) = &section.blocks[target.block] else { panic!("figure") };
    assert!(figure.images[0].href.path().ends_with("border.png"));
}

#[test]
fn worker_document_fallback_matches_catalog_scope_and_invalid_link_rules() {
    let source = source(&[
        "<html><body><p>See <a href='s1.xhtml#position'>Figure 8.2</a>, <a href='s1.xhtml'>Table 8.3</a>, <a href='s1.xhtml#missing'>Figure 8.4</a>, <a href='s1.xhtml#other'>Figure 8.4</a>, <a href='s1.xhtml#position'>Figure 8.5</a> and <a href='s1.xhtml#position'>Figure 9.5</a>.</p><figure><img src='local.png'/><figcaption>Figure 8.2 Local</figcaption></figure></body></html>",
        "<html><body><p id='position'>Ordinary prose.</p><figure><img src='border.png'/><figcaption>Figure 8.2 Border</figcaption></figure><table><caption>Table 8.3 Results</caption><tr><td>Data</td></tr></table><figure id='other'><img src='other.png'/><figcaption>Figure 8.1 Other</figcaption></figure><figure><img src='unique.png'/><figcaption>Figure 8.4 Unique</figcaption></figure><figure><img src='a.png'/><figcaption>Figure 8.5 First</figcaption></figure><figure><img src='b.png'/><figcaption>Figure 8.5 Second</figcaption></figure></body></html>",
        "<html><body><figure><img src='remote.png'/><figcaption>Figure 9.5 Elsewhere</figcaption></figure></body></html>",
    ]);
    let text = source.sections[0].blocks[0].text_blocks().next().unwrap();
    let plain = plain(text);
    for reference in media_references::inline_references(&text.content) {
        let mut request = job(source.clone(),0,"Figure 8.2");
        request.key = reference.key.clone();
        request.origin = text.source.clone().unwrap();
        request.origin.start.text_offset += plain[..reference.range.start].chars().count() as u64;
        request.origin.end.text_offset = text.source.as_ref().unwrap().start.text_offset + plain[..reference.range.end].chars().count() as u64;
        let result = Index::default().resolve(&request, &|| true);
        if ["8.2", "8.3"].contains(&reference.key.number.as_str()) {
            assert_eq!(result.unwrap().section, 1);
        } else {
            assert!(result.is_err(), "{:?} must not fall back outside its valid document/anchor scope", reference.key);
        }
    }
    assert_eq!(source.resources.load(Ordering::Relaxed), 0);
}

#[test]
#[ignore = "requires original BOOKS without barriers chapter-8.xhtml via TORTO_MEDIA_CHAPTER"]
fn local_book_page_location_resolves_the_original_and_translated_figure_8_2_reference() {
    let html = std::fs::read_to_string(std::env::var("TORTO_MEDIA_CHAPTER").unwrap()).unwrap().replace("chapter-8.xhtml", "s0.xhtml");
    let original = source(&[&html]);
    let catalog = Arc::new(super::super::media_links::Source::new(original.clone(), original.clone()));
    catalog.prepare_for_test();
    let text = original.sections[0].blocks.iter().flat_map(Block::text_blocks).find(|text| plain(text).contains("purely decorative") && plain(text).contains("Figure 8.2")).unwrap();
    let origin = text.source.as_ref().unwrap();
    let candidates = catalog.preview_references(origin, &text.content);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].0.number, "8.2");
    let mut translated = vec![Inline::Text(rebook_publication::TextRun { text: "图片纯粹用作装饰，例如文字周围的边框（图8.2）。".into(), style: Default::default(), link: None })];
    media_references::restrict_translated_links(&mut translated, &text.content);
    catalog.prepare_media_references(&mut translated, Some(origin));
    assert_eq!(catalog.preview_references(origin, &translated)[0].1, candidates[0].1);
    for supplied in [None, Some(catalog.clone())] {
        let mut request = job(original.clone(),0,"Figure 8.2");
        request.origin = origin.clone();
        request.catalog = supplied;
        assert_eq!(Index::default().resolve(&request, &|| true).unwrap(), candidates[0].1);
    }
    let Block::Figure(figure) = &original.sections[0].blocks[candidates[0].1.block] else { panic!("figure") };
    assert!(figure.caption_blocks().any(|text| plain(text).contains("Figure 8.2")));
    assert!(translated.iter().flat_map(Inline::text_runs).any(|run| run.text == "图8.2" && !run.style.suppress_media_references));
    println!("Figure 8.2 and 图8.2 resolve to the same figure, source={:?}", figure.source);
    assert_eq!(original.resources.load(Ordering::Relaxed), 0);
}

#[test]
fn numbered_table_shortcut_previews_itself_and_missing_references_do_not_open_a_popup() {
    let mut reader = reference_reader("<p>See Figure 99.</p>");
    let ctx = egui::Context::default();
    preview_key(&mut reader,&ctx,egui::Key::Num0);
    assert!(reader.media_preview.active.is_none());
    let index = reader.focus_units.iter().position(|u|u.is_table && u.media_key.as_ref().is_some_and(|key| key.number == "2")).unwrap();
    reader.select_focus_unit(index);
    preview_key(&mut reader,&ctx,egui::Key::Num0);
    let active = reader.media_preview.active.as_ref().unwrap();
    assert_eq!(active.items.len(),1);
    assert_eq!(active.key.kind,MediaKind::Table);
    assert_eq!(active.key.number,"2");
}

#[test]
fn preview_motion_reverses_continuously_and_stops_scheduling_when_idle_or_unfocused() {
    let ctx = egui::Context::default();
    let mut motion = PreviewMotion { opened_at: 0.0, closing: None };
    let frame = |motion: &mut PreviewMotion, time, focused, close| {
        let mut opacity = 0.0;
        let mut output = ctx.run_ui(egui::RawInput { time: Some(time), focused, ..Default::default() }, |_| {
            if close { motion.close(&ctx); }
            opacity = motion.opacity(&ctx);
        });
        output.textures_delta.clear();
        (opacity, output.viewport_output[&egui::ViewportId::ROOT].repaint_delay)
    };
    assert_eq!(frame(&mut motion, 0.0, true, false).0, 0.0);
    let halfway = frame(&mut motion, 0.09, true, false);
    assert!((halfway.0 - 0.5).abs() < 0.001);
    assert!(halfway.1 <= std::time::Duration::from_millis(16));
    assert_eq!(frame(&mut motion, 0.09, true, true).0, halfway.0);
    assert!((frame(&mut motion, 0.18, true, false).0 - 0.25).abs() < 0.001);
    assert_eq!(frame(&mut motion, 0.30, true, false).0, 0.0);
    frame(&mut motion, 0.5, true, false);
    assert_eq!(frame(&mut motion, 0.7, true, false).1, std::time::Duration::MAX);
    motion = PreviewMotion { opened_at: 0.7, closing: None };
    frame(&mut motion, 0.72, false, false);
    assert_eq!(frame(&mut motion, 0.74, false, false).1, std::time::Duration::MAX);
    assert_eq!(frame(&mut motion, 1.0, true, false).0, 1.0);
    frame(&mut motion, 1.2, true, false);
    assert_eq!(frame(&mut motion, 1.4, true, false).1, std::time::Duration::MAX);
}

#[test]
fn media_modal_rejects_stale_pixels_and_restores_parent_state_on_escape() {
    let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
    reader.ui.focus_footnotes_visible = true;
    let scroll_key = egui::Id::new("saved-footnote-scroll");
    reader
        .ui
        .focus_footnote_scroll_positions
        .insert(scroll_key, 123.0);
    let origin = rebook_publication::SourceRange {
        start: rebook_publication::SourceAnchor {
            spine: rebook_publication::SpineItemId::new("s").unwrap(),
            node: "p".into(),
            text_offset: 0,
        },
        end: rebook_publication::SourceAnchor {
            spine: rebook_publication::SpineItemId::new("s").unwrap(),
            node: "p".into(),
            text_offset: 10,
        },
    };
    let key = media_references::references("Figure 4.4")[0].key.clone();
    reader.media_preview.active = Some(Preview {
        motion: PreviewMotion { opened_at: 0.0, closing: None },
        content_motion: None,
        origin,
        key,
        texture: None,
        captions: vec![],
        caption_textures: vec![],
        size: egui::Vec2::ZERO,
        zoom: 1.0,
        pan: egui::Vec2::ZERO,
        previous: None,
        next: None,
        error: None,
        loading: true,
        items: vec![], item_index: 0, displayed_index: 0, position: String::new(), multipart: false,
    });
    reader.media_preview.restore_parent = true;
    let (tx, _jobs) = mpsc::sync_channel(1);
    let result = Arc::new(arc_swap::ArcSwapOption::empty());
    reader.media_preview.worker = Some(Worker {
        tx,
        pending: Arc::new(arc_swap::ArcSwapOption::empty()),
        result: result.clone(),
        generation: Arc::new(AtomicU64::new(7)),
    });
    let completed = |generation| Completion {
        generation,
        revision: reader.source.content_revision(),
        result: Ok(Ready {
            image: egui::ColorImage::filled([320, 200], egui::Color32::WHITE),
            size: [320, 200],
            captions: vec!["Figure 4.4 Caption".into()],
            caption_images: vec![egui::ColorImage::filled([280, 100], egui::Color32::WHITE)],
            previous: None,
            next: None,
            position: "1 / 1".into(),
        }),
    };
    result.store(Some(Arc::new(completed(6))));
    let current = completed(7);
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    fonts.families.insert(
        egui::FontFamily::Name(egui_commonmark_backend::STRONG_FONT_FAMILY.into()),
        fonts.families[&egui::FontFamily::Proportional].clone(),
    );
    ctx.set_fonts(fonts);
    let viewport = std::cell::Cell::new(egui::vec2(900.0, 700.0));
    let frame = |reader: &mut super::super::DesktopReader, events| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    viewport.get(),
                )),
                events,
                ..Default::default()
            },
            |_ui| {
                reader.media_preview_overlay(&ctx);
            },
        );
        output.textures_delta.clear();
        output.shapes
    };
    let _ = frame(&mut reader, vec![]);
    assert!(
        reader
            .media_preview
            .active
            .as_ref()
            .unwrap()
            .texture
            .is_none()
    );
    result.store(Some(Arc::new(current)));
    let _ = frame(&mut reader, vec![]);
    assert!(
        reader
            .media_preview
            .active
            .as_ref()
            .unwrap()
            .texture
            .is_some()
    );
    assert_eq!(
        reader.media_preview.active.as_ref().unwrap().captions,
        ["Figure 4.4 Caption"]
    );
    let active = reader.media_preview.active.as_ref().unwrap();
    let image_id = active.texture.as_ref().unwrap().id();
    let caption_id = active.caption_textures[0].id();
    let geometry = |shapes: Vec<egui::epaint::ClippedShape>| {
        fn collect(shape: &egui::Shape, meshes: &mut Vec<(egui::TextureId, egui::Rect)>) {
            match shape {
                egui::Shape::Mesh(mesh) => meshes.push((mesh.texture_id, mesh.calc_bounds())),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, meshes);
                    }
                }
                egui::Shape::Text(_) => panic!("single-part previews must have no toolbar labels"),
                _ => {}
            }
        }
        let mut meshes = Vec::new();
        for clipped in shapes {
            collect(&clipped.shape, &mut meshes);
        }
        let image = meshes.iter().find(|(id, _)| *id == image_id).unwrap().1;
        let caption = meshes.iter().find(|(id, _)| *id == caption_id).unwrap().1;
        assert!((image.bottom() - caption.top()).abs() < 0.01, "caption must touch the image's paper");
        assert!((image.center().x - caption.center().x).abs() < 0.01);
        (image, caption)
    };
    let normal_shapes = frame(&mut reader, vec![]);
    let alpha = |id| normal_shapes.iter().find_map(|shape| match &shape.shape {
        egui::Shape::Mesh(mesh) if mesh.texture_id == id => Some(mesh.vertices[0].color.a()),
        _ => None,
    }).unwrap();
    assert_eq!(alpha(image_id), alpha(caption_id), "media and caption fade together");
    assert!((1..255).contains(&alpha(image_id)), "async media content fades in when ready");
    let normal = geometry(normal_shapes);
    let fitted = normal.0.union(normal.1).size();
    let available = crate::ui::overlay_rect(&ctx).shrink(32.0).size().min(egui::vec2(1200.0, 900.0));
    assert!(normal.0.width() > 320.0, "small media should expand to the window");
    assert!(fitted.x <= available.x + 0.01 && fitted.y <= available.y + 0.01);
    assert!((fitted.x - available.x).abs() < 0.01 || (fitted.y - available.y).abs() < 0.01);
    reader.media_preview.active.as_mut().unwrap().zoom = 4.0;
    let zoomed = geometry(frame(&mut reader, vec![]));
    assert!((zoomed.0.width() / normal.0.width() - 4.0).abs() < 0.01);
    assert!((zoomed.1.width() / normal.1.width() - 4.0).abs() < 0.01);
    reader.media_preview.active.as_mut().unwrap().pan = egui::vec2(60.0, 40.0);
    let panned = geometry(frame(&mut reader, vec![]));
    assert!((panned.0.min - zoomed.0.min - egui::vec2(60.0, 40.0)).length() < 0.01);
    assert!((panned.1.min - zoomed.1.min - egui::vec2(60.0, 40.0)).length() < 0.01);
    let active = reader.media_preview.active.as_mut().unwrap();
    active.zoom = 1.0;
    active.pan = egui::Vec2::ZERO;
    viewport.set(egui::vec2(1200.0, 1000.0));
    let resized = geometry(frame(&mut reader, vec![]));
    let fitted = resized.0.union(resized.1).size();
    let available = crate::ui::overlay_rect(&ctx).shrink(32.0).size().min(egui::vec2(1200.0, 900.0));
    assert!(resized.0.width() > normal.0.width());
    assert!(fitted.x <= available.x + 0.01 && fitted.y <= available.y + 0.01);
    assert!((fitted.x - available.x).abs() < 0.01 || (fitted.y - available.y).abs() < 0.01);
    let mut output = ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        },
        |_ui| {
            assert!(reader.media_preview_keyboard(&ctx));
        },
    );
    output.textures_delta.clear();
    assert!(reader.media_preview.active.as_ref().unwrap().motion.is_closing());
    assert!(reader.preview_is_open(), "retain input ownership during fade-out");
    let now = ctx.input(|i| i.time);
    let mut output = ctx.run_ui(egui::RawInput { time: Some(now + 0.2), ..Default::default() }, |_| reader.media_preview_overlay(&ctx));
    assert!(reader.media_preview.active.is_none());
    assert!(!output.textures_delta.free.is_empty(), "release preview and caption textures after fading");
    output.textures_delta.clear();
    assert!(reader.ui.focus_footnotes_visible);
    assert_eq!(
        reader.ui.focus_footnote_scroll_positions[&scroll_key],
        123.0
    );
    assert!(reader.preserve_media_parent_popup());
    assert!(result.load_full().is_none());
    reader.ui.focus_footnotes_visible = false;
    assert!(!reader.preserve_media_parent_popup());
    reader.ui.focus_footnotes_visible = true;
    assert!(!reader.preserve_media_parent_popup(), "a later hover popup must regain its ordinary dismissal behavior");
}

struct Source {
    book: Book,
    sections: Vec<Section>,
    resources: AtomicUsize,
    parses: AtomicUsize,
}
impl BookSource for Source {
    fn book(&self) -> &Book {
        &self.book
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        self.parses.fetch_add(1, Ordering::Relaxed);
        Ok(self.sections[index].clone())
    }
    fn resource(&self, _: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.resources.fetch_add(1, Ordering::Relaxed);
        Err(PublicationError::InvalidPublication(
            "No image should be loaded by the index".into(),
        ))
    }
}
fn source(html: &[&str]) -> Arc<Source> {
    let descriptors: Vec<_> = html
        .iter()
        .enumerate()
        .map(|(i, _)| SpineItem {
            id: SpineItemId::new(format!("s{i}")).unwrap(),
            href: PublicationUrl::parse(&format!("s{i}.xhtml")).unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        })
        .collect();
    let sections = html
        .iter()
        .zip(&descriptors)
        .map(|(html, descriptor)| rebook_html::parse_section(html, descriptor, |_| None).unwrap())
        .collect();
    Arc::new(Source {
        book: Book {
            id: PublicationId::new("media-tests").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: descriptors,
            table_of_contents: vec![],
        },
        sections,
        resources: AtomicUsize::new(0),
        parses: AtomicUsize::new(0),
    })
}
fn job(source: Arc<dyn BookSource>, at: usize, key: &str) -> Job {
    let section = source.parse_section(at).unwrap();
    let origin = section
        .blocks
        .iter()
        .flat_map(text_blocks)
        .find_map(|t| t.source.clone())
        .unwrap();
    Job {
        generation: 1,
        source,
        catalog: None,
        destination: None,
        origin,
        key: media_references::references(key)[0].key.clone(),
        cursor: Cursor::default(),
        style: ReaderStyle::default(),
        ctx: egui::Context::default(),
    }
}
#[test]
fn lookup_uses_links_and_local_scope_without_loading_images() {
    let source = source(&[
        "<html><body><p>See <a href='s1.xhtml#target'>Figure 1</a>.</p><figure><img src='a.png'/><figcaption>Figure 1 Local</figcaption></figure></body></html>",
        "<html><body><figure id='target'><img src='b.png'/><img src='c.png'/><figcaption>Figure 1 Linked complete group</figcaption></figure></body></html>",
    ]);
    let target = Index::default()
        .resolve(&job(source.clone(), 0, "Figure 1"), &|| true)
        .unwrap();
    assert_eq!(
        target.section, 1,
        "the authored link wins over the local duplicate"
    );
    assert_eq!(source.resources.load(Ordering::Relaxed), 0);
    let local = Index::default()
        .resolve(&job(source.clone(), 1, "Figure 1"), &|| true)
        .unwrap();
    assert_eq!(local.section, 1);
}

#[test]
fn structured_figures_preview_text_and_tables_with_caption_and_source() {
    let source=source(&["<html><body><p>See Figure 4.3.</p><div><ol start='6'><li><p>Solve x = 2</p></li></ol><table><tr><td>Text cell</td></tr></table></div><p>Figure 4.3 Nested example</p><p>Source: Authors.</p></body></html>"]);
    let request=job(source.clone(),0,"Figure 4.3");
    let selected=Index::default().resolve(&request,&||true).unwrap();
    assert_eq!(source.resources.load(Ordering::Relaxed),0);
    let result=render(&request,selected,&||true).unwrap();
    assert_eq!(result.captions,["Figure 4.3 Nested example","Source: Authors."]);
    assert!(result.image.pixels.len()*4<=PIXEL_BUDGET);
    assert!(result.size[1]>80);
    assert!(result.image.pixels.iter().any(|p|p.r()<100));
    assert_eq!(source.resources.load(Ordering::Relaxed),0);
}
#[test]
fn short_text_figure_preview_fits_its_padded_body() {
    let source = source(&["<html><body><p>See Figure 2.1.</p><figure><div><p>Short diagram</p><p style='text-align:right'>Right aligned</p></div><figcaption>Figure 2.1 Text example</figcaption></figure></body></html>"]);
    let request = job(source.clone(), 0, "Figure 2.1");
    let selected = Index::default().resolve(&request, &|| true).unwrap();
    let result = render(&request, selected, &|| true).unwrap();
    assert!(result.size[0] < 600, "short diagrams must not retain a full-width blank column");
    assert!(result.size[1] > 70);
    assert_eq!(result.image.pixels[0], egui::Color32::WHITE);
    assert!(result.image.pixels.iter().any(|p| p.r() < 100));
    assert_eq!(result.captions, ["Figure 2.1 Text example"]);
    assert_eq!(source.resources.load(Ordering::Relaxed), 0);
}
#[test]
fn ambiguous_global_numbers_and_list_of_figures_never_pick_a_target() {
    let source = source(&[
        "<html><body><p>See Figure 1.</p><p>Figure 1 list entry</p></body></html>",
        "<html><body><figure><img src='a.png'/><figcaption>Figure 1 First</figcaption></figure></body></html>",
        "<html><body><figure><img src='b.png'/><figcaption>Figure 1 Second</figcaption></figure></body></html>",
    ]);
    assert!(targets(&source.sections[0], 0).is_empty());
    assert!(
        Index::default()
            .resolve(&job(source.clone(), 0, "Figure 1"), &|| true)
            .unwrap_err()
            .contains("several")
    );
    assert_eq!(source.resources.load(Ordering::Relaxed), 0);
}
#[test]
fn long_table_chunks_keep_spanned_rows_together_and_render_all_segments() {
    let rows = (0..80)
        .map(|i| {
            format!(
                "<tr><td{}>Row {i}</td><td>value</td></tr>",
                if i == 23 { " rowspan='4'" } else { "" }
            )
        })
        .collect::<String>();
    let html = format!(
        "<html><body><p>Table 3.1 Values</p><table>{rows}</table><p class='table-note'>Note: Complete trailing note.</p></body></html>"
    );
    let source = source(&[&html]);
    let target = targets(&source.sections[0], 0).pop().unwrap();
    let Block::Table(table) = &source.sections[0].blocks[target.block] else {
        panic!("table")
    };
    let chunks = table_chunks(table).unwrap();
    assert_eq!(chunks[0], 0..27);
    assert_eq!(chunks.last().unwrap().end, 80);
    let mut job = job(source, 0, "Table 3.1");
    let mut seen = 0;
    loop {
        let ready = render(&job, target.clone(), &|| true).unwrap();
        assert!(ready.image.pixels.len() * 4 <= PIXEL_BUDGET);
        assert!(ready.captions.join(" ").contains("Complete trailing note"));
        seen += 1;
        if let Some(next) = ready.next {
            job.cursor = next;
        } else {
            break;
        }
        assert!(seen < 12);
    }
    assert!(seen >= chunks.len());
}
#[test]
fn wrapped_reference_geometry_keeps_source_and_follows_vertical_corrections() {
    let source =
        source(&["<html><body><p>See Figure 12.34 and Table 3.1 for details.</p></body></html>"]);
    let mut style = ReaderStyle::default();
    style.typesetting.mode = rebook_layout::TypesettingMode::Unified;
    style.horizontal_margin = 10.0;
    style.minimum_horizontal_margin = 0.0;
    style.minimum_content_width = 0.0;
    style.spread = SpreadMode::Single;
    let layout = LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned())
        .layout_section(
            source.as_ref(),
            &source.sections[0],
            LayoutViewport::new(240, 400).unwrap(),
            &style,
        )
        .unwrap();
    let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
    let mut count = 0;
    for item in &layout.pages[0].items {
        if let rebook_layout::PageItem::Text(text) = item {
            for line in text.layout.lines() {
                for item in line.items() {
                    if let rebook_layout::text_layout::PositionedLayoutItem::GlyphRun(run) = item {
                        let brush = run.style().brush;
                        if brush.color.blue > brush.color.red {
                            assert!(!brush.underline, "clickable media links need no underline");
                        }
                    }
                }
            }
            for (rect, key) in rebook_renderer::text_media_reference_rects(text) {
                let at = rect.center();
                let hit = display
                    .media_reference_at(at.x as f32, at.y as f32)
                    .unwrap();
                assert_eq!(hit.key, key);
                assert_eq!(
                    hit.source.start.node,
                    text.source.as_ref().unwrap().start.node
                );
                let shifted = display.translate_source_text(&hit.source, 37.0);
                assert_eq!(
                    shifted
                        .media_reference_at(at.x as f32, at.y as f32 + 37.0)
                        .unwrap()
                        .source,
                    hit.source
                );
                count += 1;
            }
        }
    }
    assert!(count >= 2);
}
#[test]
fn cancelling_a_lookup_does_not_scan_or_decode_the_book() {
    let source = source(&[
        "<html><body><p>See Figure 9.</p></body></html>",
        "<html><body><p>Other chapter.</p></body></html>",
    ]);
    let request = job(source.clone(), 0, "Figure 9");
    let before = source.parses.load(Ordering::Relaxed);
    assert!(Index::default().resolve(&request, &|| false).is_err());
    assert_eq!(source.parses.load(Ordering::Relaxed) - before, 1);
    assert_eq!(source.resources.load(Ordering::Relaxed), 0);
}

#[test]
fn adjacent_superscript_note_preserves_media_hit_number_source_and_authored_target() {
    let source = source(&[
        "<html xmlns:epub='http://www.idpf.org/2007/ops'><body><p>At <a href='https://example.com'>the website</a>, shown in <a href='s1.xhtml#f13'>Fig. 11.13</a><a epub:type='noteref' href='#fn2'><sup>2</sup></a>.</p></body></html>",
        "<html><body><figure id='f13'><img src='a.png'/><figcaption>Figure 11.13 Target caption.</figcaption></figure></body></html>",
    ]);
    let mut style = ReaderStyle::default();
    style.typesetting.mode = rebook_layout::TypesettingMode::Unified;
    style.spread = SpreadMode::Single;
    style.website_icons = true;
    for focus in [false, true] {
        style.focus_footnote_icons = focus;
        let layout = LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned()).layout_section(source.as_ref(), &source.sections[0], LayoutViewport::new(600, 400).unwrap(), &style).unwrap();
        let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
        let mut found = 0;
        for item in &layout.pages[0].items {
            let rebook_layout::PageItem::Text(text) = item else { continue };
            for (rect, key) in rebook_renderer::text_media_reference_rects(text) {
                assert_eq!(key.number, "11.13", "the superscript is a separate note, not part of the figure number");
                let at = rect.center();
                let hit = display.media_reference_at(at.x as f32, at.y as f32).unwrap();
                assert_eq!(hit.key, key);
                let mut request = job(source.clone(), 0, "Figure 11.13");
                request.origin = hit.source;
                let target = Index::default().resolve(&request, &|| true).unwrap();
                assert_eq!(target.section, 1);
                found += 1;
            }
        }
        assert!(found > 0);
    }
    assert_eq!(source.resources.load(Ordering::Relaxed), 0);
}

#[test]
fn unverified_translation_reference_cannot_borrow_a_nearby_links_shaped_run() {
    let source = source(&["<html><body><p>See Figure 1. This original paragraph has enough ordinary prose to keep all translated source offsets within its range and does not mention any tables.</p></body></html>"]);
    let mut section = source.sections[0].clone();
    let Block::Text(block) = &mut section.blocks[0] else { panic!("paragraph") };
    let original = block.content.clone();
    block.content = vec![Inline::Text(rebook_publication::TextRun {
        text: "See Figure 1. Translation invents Table 99.".into(),
        style: Default::default(),
        link: None,
    })];
    media_references::restrict_translated_links(&mut block.content, &original);
    let mut style = ReaderStyle::default();
    style.typesetting.mode = rebook_layout::TypesettingMode::Unified;
    style.spread = SpreadMode::Single;
    let layout = LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned()).layout_section(source.as_ref(), &section, LayoutViewport::new(600, 400).unwrap(), &style).unwrap();
    let mut keys = Vec::new();
    for page in &layout.pages {
        for item in &page.items {
            let rebook_layout::PageItem::Text(text) = item else { continue };
            keys.extend(rebook_renderer::text_media_reference_rects(text).into_iter().map(|(_, key)| key));
        }
    }
    assert!(!keys.is_empty());
    assert!(keys.iter().all(|key| key.kind == MediaKind::Figure && key.number == "1"), "uncolored Table 99 must not acquire invisible click geometry: {keys:?}");
}
#[test]
#[ignore = "requires TORTO_MEDIA_BOOK, optionally TORTO_MEDIA_KEY and TORTO_MEDIA_PNG"]
fn local_book_media_preview() {
    let publication =
        rebook_formats::open_file(std::env::var("TORTO_MEDIA_BOOK").unwrap()).unwrap();
    let original = publication.source();
    let catalog = Arc::new(super::super::media_links::Source::new(original.clone(), original));
    catalog.prepare_for_test();
    let source: Arc<dyn BookSource> = catalog.clone();
    let wanted = media_references::references(
        &std::env::var("TORTO_MEDIA_KEY").unwrap_or("Figure 4.5".into()),
    )[0]
    .key
    .clone();
    let mut all = Vec::new();
    for at in 0..source.book().sections.len() {
        all.extend(targets(&source.parse_section(at).unwrap(), at));
    }
    println!("{} media targets", all.len());
    let selected = all
        .iter()
        .find(|t| t.key == wanted)
        .expect("requested media target")
        .clone();
    let section = source.parse_section(selected.section).unwrap();
    println!(
        "target: {selected:?}; block: {:?}",
        std::mem::discriminant(&section.blocks[selected.block])
    );
    let origin = section
        .blocks
        .iter()
        .flat_map(text_blocks)
        .find_map(|t| t.source.clone())
        .unwrap();
    let mut request = Job {
        generation: 1,
        source,
        catalog: Some(catalog),
        destination: None,
        origin,
        key: wanted,
        cursor: Cursor::default(),
        style: ReaderStyle::default(),
        ctx: egui::Context::default(),
    };
    // Exercise the real body's shaped click range, including neighboring note
    // markers and website placeholders, rather than looking up the label alone.
    if let Some(body) = section.blocks.iter().flat_map(text_blocks).find(|text| {
        text.kind != rebook_publication::TextBlockKind::Caption
            && media_references::caption_label(&plain(text)).is_none()
            && media_references::inline_references(&text.content).iter().any(|r| r.key == request.key && r.range.start > 0)
    }) {
        let mut click_section = section.clone();
        click_section.blocks = vec![Block::Text(body.clone())];
        let mut style = ReaderStyle::default();
        style.typesetting.mode = rebook_layout::TypesettingMode::Unified;
        style.focus_footnote_icons = true;
        style.spread = SpreadMode::Single;
        let layout = LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned()).layout_section(request.source.as_ref(), &click_section, LayoutViewport::new(900, 600).unwrap(), &style).unwrap();
        let mut hits = Vec::new();
        for page in &layout.pages {
            let display = rebook_renderer::DisplayListCompiler.compile(page);
            for item in &page.items {
                let rebook_layout::PageItem::Text(text) = item else { continue };
                for (rect, key) in rebook_renderer::text_media_reference_rects(text) {
                    println!("actual body media hit: {key:?}");
                    if key == request.key {
                        let at = rect.center();
                        hits.push(display.media_reference_at(at.x as f32, at.y as f32).unwrap());
                    }
                }
            }
        }
        request.origin = hits.first().expect("the original body reference must remain clickable").source.clone();
    }
    let resolved = Index::default().resolve(&request, &|| true).unwrap();
    assert_eq!(resolved.block, selected.block);
    let mut count = 0;
    loop {
        let ready = render(&request, selected.clone(), &|| true).unwrap();
        println!(
            "page {}: {}x{} bytes={} captions={:?}",
            ready.position,
            ready.size[0],
            ready.size[1],
            ready.image.pixels.len() * 4,
            ready.captions
        );
        if request.key.kind != MediaKind::Equation { assert!(!ready.captions.is_empty()); }
        assert!(ready.image.pixels.len() * 4 <= PIXEL_BUDGET);
        if count == 0
            && let Ok(path) = std::env::var("TORTO_MEDIA_PNG")
        {
            image::RgbaImage::from_raw(
                ready.size[0] as u32,
                ready.size[1] as u32,
                ready
                    .image
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect(),
            )
            .unwrap()
            .save(&path)
            .unwrap();
            if let Some(caption) = ready.caption_images.first() {
                image::RgbaImage::from_raw(
                    caption.size[0] as u32,
                    caption.size[1] as u32,
                    caption
                        .pixels
                        .iter()
                        .flat_map(|pixel| pixel.to_array())
                        .collect(),
                )
                .unwrap()
                .save(format!("{path}.caption.png"))
                .unwrap();
            }
        }
        count += 1;
        if let Some(next) = ready.next {
            request.cursor = next;
        } else {
            break;
        }
        assert!(count < 100);
    }
    if let Block::Figure(figure) = &section.blocks[selected.block]
        && figure.body.is_empty()
    {
        assert_eq!(
            count,
            figure.images.len(),
            "all authored media images remain accessible; inline caption formulas stay in the caption"
        );
    }
}
