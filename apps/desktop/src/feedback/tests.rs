use super::*;

fn draft() -> Draft {
    let seed = OpenContext::shelf(&egui::Context::default());
    Draft {
        version: 1,
        endpoint: transport::DEFAULT_ENDPOINT.into(),
        retry_until: None,
        payload: Payload {
            request_id: uuid::Uuid::new_v4().to_string(),
            kind: Kind::Bug,
            description: "统一版式下出现重复的表格背景。".into(),
            screenshots: Vec::new(),
            context: seed.context,
        },
    }
}

#[test]
fn character_validation_matches_the_service() {
    assert!(!valid_description("一二三四五六七八九"));
    assert!(valid_description("一 二 三 四 五 六 七 八 九 十"));
    assert_eq!(character_count(&"e\u{301}".repeat(10)), 10);
    assert_eq!(character_count(&"👨‍👩‍👧‍👦".repeat(9)), 9);
    assert!(!valid_description(&"\u{200b}".repeat(10)));
    assert!(!valid_description(&"字".repeat(5001)));
    assert!(!valid_description(&"👨‍👩‍👧‍👦".repeat(1000)));
}

#[test]
fn opening_waits_for_context_and_restored_images_before_painting_a_stable_form() {
    for size in [egui::vec2(1200.0, 900.0), egui::vec2(900.0, 600.0)] {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
        let mut feature = FeedbackFeature::default();
        feature.open(OpenContext::shelf(&ctx));
        let mut time = 0.0;
        let mut render = |feature: &mut FeedbackFeature| {
            time += 1.0 / 60.0;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    time: Some(time),
                    ..Default::default()
                },
                |ui| feature.overlay(ui.ctx(), AppLanguage::English),
            );
            let has_text = output.shapes.iter().any(|shape| {
                matches!(shape.shape, egui::Shape::Text(_))
            });
            output.textures_delta.clear();
            let rect = ctx.memory(|memory| memory.area_rect(egui::Id::new("feedback-modal"))).unwrap();
            (has_text, rect)
        };
        for _ in 0..5 {
            assert!(!render(&mut feature).0, "must not show an empty dialog while preparing");
        }
        let pending = feature.task.take_pending().unwrap();
        let image = screenshots::encode(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255])),
        )).unwrap();
        let mut report = draft();
        report.payload.screenshots.push(image.screenshot.clone());
        feature.complete(TaskResult {
            id: pending.id,
            result: Ok(Output::Ready(report, true)),
        });
        for _ in 0..5 {
            assert!(!render(&mut feature).0, "restored screenshot placeholders must not resize the visible dialog");
        }
        let pending = feature.image_task.take_pending().unwrap();
        feature.complete_images(TaskResult { id: pending.id, result: Ok(vec![image]) });
        let mut first_visible = None;
        for _ in 0..10 {
            let (painted, rect) = render(&mut feature);
            if painted {
                let first = *first_visible.get_or_insert(rect);
                assert!((rect.size() - first.size()).abs().max_elem() < 0.5, "{first:?} -> {rect:?}");
                let center = crate::ui::overlay_rect(&ctx).center() + egui::vec2(0.0, (1.0 - feature.visibility) * 12.0);
                assert!(rect.center().distance(center) < 1.0);
            }
        }
        assert!(first_visible.is_some(), "must finish sizing and show the form");
        // Reopening uses the existing Area state, which must not expose its old
        // height while new context is still being prepared.
        feature.visible = false;
        feature.locked = false;
        feature.open(OpenContext::shelf(&ctx));
        for _ in 0..5 {
            assert!(!render(&mut feature).0);
        }
    }
}

#[test]
fn feedback_motion_preserves_size_and_blocks_input_until_the_exit_finishes() {
    let ctx = egui::Context::default();
    ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
    let mut feature = FeedbackFeature { draft: Some(draft()), ..Default::default() };
    let render = |feature: &mut FeedbackFeature, time, events| {
        let mut output = ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 900.0))),
            time: Some(time), events, ..Default::default()
        }, |ui| feature.overlay(ui.ctx(), AppLanguage::English));
        let painted = !output.shapes.is_empty();
        let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        output.textures_delta.clear();
        (painted, delay, ctx.memory(|memory| memory.area_rect(egui::Id::new("feedback-modal"))))
    };
    assert!(!render(&mut feature, 0.0, vec![]).0);
    feature.visible = true;
    let mut size = None;
    let mut saw_opening = false;
    for frame in 1..=24 {
        let (_, _, rect) = render(&mut feature, frame as f64 * 0.02, vec![]);
        if feature.visibility > f32::EPSILON {
            let rect = rect.unwrap();
            let original = *size.get_or_insert(rect.size());
            assert!((rect.size() - original).abs().max_elem() < 0.5);
            let center = crate::ui::overlay_rect(&ctx).center() + egui::vec2(0.0, (1.0 - feature.visibility) * 12.0);
            assert!(rect.center().distance(center) < 1.0);
            saw_opening |= feature.visibility < 1.0;
        }
    }
    assert!(saw_opening, "the measured form must animate into view");
    assert_eq!(feature.visibility, 1.0);
    assert_eq!(render(&mut feature, 0.5, vec![]).1, Duration::MAX);
    let escape = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    render(&mut feature, 0.52, vec![escape]);
    assert!(!feature.visible && feature.is_open(), "closing keeps the modal input barrier until the animation ends");
    let original = serde_json::to_value(&feature.draft.as_ref().unwrap().payload).unwrap();
    render(&mut feature, 0.54, vec![enter(egui::Modifiers::NONE, false)]);
    assert!(feature.visibility > 0.0 && feature.visibility < 1.0);
    assert!(!feature.task.is_pending(), "closing content must not accept a submission");
    let mut released = false;
    for frame in 28..=44 {
        let was_open = feature.is_open();
        let (_, delay, _) = render(&mut feature, frame as f64 * 0.02, vec![]);
        if was_open && !feature.is_open() {
            released = true;
            assert_eq!(delay, Duration::ZERO, "refresh the parent after releasing its input barrier");
        }
    }
    assert!(released);
    assert!(!feature.is_open());
    let closed = render(&mut feature, 0.9, vec![]);
    assert!(!closed.0);
    assert_eq!(closed.1, Duration::MAX);
    assert_eq!(serde_json::to_value(&feature.draft.as_ref().unwrap().payload).unwrap(), original);
}

#[test]
fn submitted_dialog_shrinks_and_recenters_after_a_tall_form() {
    for size in [egui::vec2(1200.0, 900.0), egui::vec2(900.0, 600.0)] {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
        let mut report = draft();
        report.payload.description = "A report with multiple lines of detail.\n".repeat(30);
        let mut feature = FeedbackFeature {
            visible: true,
            loaded: true,
            draft: Some(report.clone()),
            ..Default::default()
        };
        let mut time = 0.0;
        let mut render = |feature: &mut FeedbackFeature| {
            for _ in 0..8 {
                time += 1.0 / 60.0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        time: Some(time),
                        ..Default::default()
                    },
                    |ui| feature.overlay(ui.ctx(), AppLanguage::English),
                );
                output.textures_delta.clear();
            }
            ctx.memory(|memory| memory.area_rect(egui::Id::new("feedback-modal")))
                .unwrap()
        };
        let form = render(&mut feature);
        feature.send();
        let pending = feature.task.take_pending().unwrap();
        let Work::Send(sent) = pending.payload else {
            panic!()
        };
        feature.complete(TaskResult {
            id: pending.id,
            result: Ok(Output::Sent(
                sent,
                Outcome::Submitted(transport::Issue {
                    number: 7,
                    url: "https://github.com/TortoTech/torto/issues/7".into(),
                }),
            )),
        });
        let submitted = render(&mut feature);
        assert!(
            submitted.height() < 260.0,
            "success dialog retained form height: {submitted:?}"
        );
        assert!(
            form.height() > submitted.height() + 150.0,
            "{form:?} -> {submitted:?}"
        );
        assert!((form.width() - submitted.width()).abs() < 1.0);
        let center = crate::ui::overlay_rect(&ctx).center() + egui::vec2(0.0, (1.0 - feature.visibility) * 12.0);
        assert!(submitted.center().distance(center) < 1.0);
        let settled = render(&mut feature);
        assert!((settled.height() - submitted.height()).abs() < 0.1);
        assert!(settled.center().distance(crate::ui::overlay_rect(&ctx).center()) < 1.0);

        feature.visible = false;
        feature.open(OpenContext::shelf(&ctx));
        let pending = feature.task.take_pending().unwrap();
        feature.complete(TaskResult {
            id: pending.id,
            result: Ok(Output::Ready(report, false)),
        });
        let reopened = render(&mut feature);
        assert!(
            reopened.height() > submitted.height() + 150.0,
            "new form must grow again: {reopened:?}"
        );
    }
}

#[test]
fn retry_freezes_the_exact_request_and_disallows_parallel_submission() {
    let mut original = draft();
    original.payload.screenshots.push(
        screenshots::encode(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 100, 50, 255])),
        ))
        .unwrap()
        .screenshot,
    );
    let mut feature = FeedbackFeature {
        draft: Some(original.clone()),
        ..Default::default()
    };
    feature.send();
    let first = feature.task.take_pending().unwrap();
    let Work::Send(sent) = &first.payload else {
        panic!()
    };
    assert_ne!(sent.payload.request_id, original.payload.request_id);
    feature.send();
    assert!(feature.task.take_pending().is_none());
    feature.complete(TaskResult {
        id: first.id,
        result: Ok(Output::Sent(
            sent.clone(),
            Outcome::Failed(Failure::Network, None),
        )),
    });
    feature.send();
    let second = feature.task.take_pending().unwrap();
    let Work::Send(retry) = second.payload else {
        panic!()
    };
    assert_eq!(
        serde_json::to_value(&sent.payload).unwrap(),
        serde_json::to_value(&retry.payload).unwrap()
    );
    assert_eq!(sent.endpoint, retry.endpoint);
}

fn enter(modifiers: egui::Modifiers, repeat: bool) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat,
        modifiers,
    }
}

#[test]
fn submitted_dialog_enter_closes_without_resubmitting_or_confirming_ime() {
    for (events, composing, closes) in [
        (vec![enter(egui::Modifiers::NONE, false)], false, true),
        (vec![enter(egui::Modifiers::NONE, true)], false, false),
        (vec![enter(egui::Modifiers::SHIFT, false)], false, false),
        (vec![enter(egui::Modifiers::CTRL, false)], false, false),
        (vec![enter(egui::Modifiers::NONE, false)], true, false),
        (vec![
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "中文".into(), active_range_chars: None,
            }),
            enter(egui::Modifiers::NONE, false),
        ], false, false),
        (vec![
            egui::Event::Ime(egui::ImeEvent::Commit("中文".into())),
            enter(egui::Modifiers::NONE, false),
        ], false, false),
    ] {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
        let mut feature = FeedbackFeature {
            visible: true,
            draft: Some(draft()),
            status: Status::Submitted(transport::Issue {
                number: 7,
                url: "https://github.com/TortoTech/torto/issues/7".into(),
            }),
            ..Default::default()
        };
        let mut time = 0.0;
        let mut render = |feature: &mut FeedbackFeature, events| {
            time += 1.0 / 60.0;
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 900.0))),
                time: Some(time), events, ..Default::default()
            }, |ui| feature.overlay(ui.ctx(), AppLanguage::English));
            output.textures_delta.clear();
        };
        for _ in 0..6 { render(&mut feature, Vec::new()); }
        assert!(feature.layout_ready && feature.is_open());
        feature.ime_active = composing;
        if events.iter().any(|event| matches!(event, egui::Event::Key { repeat: true, .. })) {
            ctx.input_mut(|input| { input.keys_down.insert(egui::Key::Enter); });
        }
        render(&mut feature, events);
        assert_eq!(feature.visible, !closes);
        // The backdrop keeps input blocked until the closing motion completes.
        for _ in 0..16 { render(&mut feature, Vec::new()); }
        assert_eq!(feature.is_open(), !closes);
        assert!(feature.task.take_pending().is_none(), "Done must never submit again");
        assert!(matches!(feature.status, Status::Submitted(_)));
    }
}

#[test]
fn enter_submission_preserves_shift_newlines_and_ime_confirmation() {
    let mut composing = false;
    let mut events = vec![enter(egui::Modifiers::NONE, false)];
    assert!(submit_key(&mut events, &mut composing));
    assert!(events.is_empty());
    assert!(!submit_key(
        &mut vec![enter(egui::Modifiers::NONE, true)],
        &mut composing
    ));
    let mut shift = vec![enter(egui::Modifiers::SHIFT, false)];
    assert!(!submit_key(&mut shift, &mut composing));
    assert_eq!(shift.len(), 1);
    let mut preedit = vec![egui::Event::Ime(egui::ImeEvent::Preedit {
        text: "中文".into(),
        active_range_chars: None,
    })];
    assert!(!submit_key(&mut preedit, &mut composing));
    assert!(composing);
    assert!(!submit_key(
        &mut vec![enter(egui::Modifiers::NONE, false)],
        &mut composing
    ));
    let mut commit = vec![
        egui::Event::Ime(egui::ImeEvent::Commit("中文".into())),
        enter(egui::Modifiers::NONE, false),
    ];
    assert!(!submit_key(&mut commit, &mut composing));
    assert!(!composing);
    assert!(submit_key(
        &mut vec![enter(egui::Modifiers::NONE, false)],
        &mut composing
    ));
}

#[test]
fn screenshots_use_jpeg_for_opaque_pixels_and_preserve_png_transparency() {
    use base64::Engine;
    for alpha in [255, 128, 0] {
        let mut pixels = image::RgbaImage::from_pixel(12, 8, image::Rgba([80, 50, 20, 255]));
        pixels.put_pixel(0, 0, image::Rgba([80, 50, 20, alpha]));
        let prepared = screenshots::encode(image::DynamicImage::ImageRgba8(pixels)).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(&prepared.screenshot.data).unwrap();
        assert_eq!(image::guess_format(&bytes).unwrap(), if alpha == 255 {
            image::ImageFormat::Jpeg
        } else {
            image::ImageFormat::Png
        });
        assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8().get_pixel(0, 0)[3], alpha);
        assert!(screenshots::restore(prepared.screenshot).is_ok());
    }
    // Existing opaque PNG drafts must retain their exact bytes on restore/retry.
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 6))
        .write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    let data = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
    assert_eq!(screenshots::restore(model::Screenshot { data: data.clone() }).unwrap().screenshot.data, data);
}

#[test]
fn screenshot_preview_preserves_report_and_closes_without_submitting() {
    let ctx = egui::Context::default();
    let prepared = screenshots::encode(image::DynamicImage::ImageRgb8(image::RgbImage::new(800, 1600))).unwrap();
    let screenshot = prepared.screenshot;
    let mut report = draft();
    report.payload.screenshots.push(screenshot.clone());
    let original = serde_json::to_value(&report.payload).unwrap();
    let mut feature = FeedbackFeature {
        visible: true,
        loaded: true,
        draft: Some(report),
        prepared_images: vec![prepared.thumbnail],
        ..Default::default()
    };
    let render = |feature: &mut FeedbackFeature, time, events| {
        let mut output = ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 900.0))),
            time: Some(time), events, ..Default::default()
        }, |ui| feature.overlay(ui.ctx(), AppLanguage::English));
        output.textures_delta.clear();
    };
    for frame in 0..8 { render(&mut feature, frame as f64 * 0.02, vec![]); }
    feature.open_preview(0);
    let pending = feature.preview_task.take_pending().unwrap();
    assert!(!feature.accepts_images());
    let image = tokio::runtime::Runtime::new().unwrap().block_on(screenshots::preview(screenshot)).unwrap();
    let full_size = image.size;
    assert!(full_size[0] > 160 && full_size[1] > 112);
    feature.complete_preview(TaskResult { id: pending.id, result: Ok(image) });
    render(&mut feature, 0.2, vec![]);
    assert_eq!(feature.preview.as_ref().unwrap().image.size, full_size);
    let key = |key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    render(&mut feature, 0.4, vec![key(egui::Key::Enter)]);
    assert!(!feature.task.is_pending(), "Enter in preview must never submit the report");
    render(&mut feature, 0.5, vec![key(egui::Key::Escape)]);
    assert!(feature.preview.as_ref().unwrap().motion.is_closing());
    render(&mut feature, 0.8, vec![]);
    assert!(feature.preview.is_none());
    assert!(feature.visible, "Escape closes the preview, preserving the parent dialog");
    assert_eq!(serde_json::to_value(&feature.draft.as_ref().unwrap().payload).unwrap(), original);
    feature.open_preview(0);
    let pending = feature.preview_task.take_pending().unwrap();
    feature.clear_preview();
    feature.complete_preview(TaskResult { id: pending.id, result: Ok(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)) });
    assert!(!feature.preview_active(), "cancelled decodes cannot reopen a preview");
}

#[test]
fn screenshot_thumbnail_click_and_remove_have_separate_targets() {
    let ctx = egui::Context::default();
    let texture = ctx.load_texture("screenshot-click-test", egui::ColorImage::filled([8, 6], egui::Color32::WHITE), egui::TextureOptions::LINEAR);
    let render = |events| {
        let mut result = None;
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
            let (view, remove) = screenshot_thumbnail(ui, &texture, true, AppLanguage::English);
            result = Some((view.rect, view.clicked(), remove.clicked()));
        });
        output.textures_delta.clear();
        result.unwrap()
    };
    let rect = render(vec![]).0;
    render(vec![]);
    for (point, expected) in [(rect.center(), (true, false)), (rect.right_top() + egui::vec2(-13.0, 13.0), (false, true))] {
        render(vec![egui::Event::PointerMoved(point), egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        let (_, view, remove) = render(vec![egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
        assert_eq!((view, remove), expected);
    }
}

#[test]
fn screenshot_encoding_is_bounded_and_rejects_corrupt_restored_bytes() {
    let pixels = image::RgbaImage::from_pixel(3000, 1000, image::Rgba([80, 50, 20, 255]));
    let prepared = screenshots::encode(image::DynamicImage::ImageRgba8(pixels)).unwrap();
    assert!(prepared.thumbnail.size[0] <= 160 && prepared.thumbnail.size[1] <= 112);
    let restored = screenshots::restore(prepared.screenshot).unwrap();
    assert_eq!(restored.thumbnail.size, prepared.thumbnail.size);
    assert!(
        screenshots::restore(model::Screenshot {
            data: "forged".into()
        })
        .is_err()
    );
}

#[test]
fn screenshot_batches_share_the_three_image_limit_and_reject_stale_completions() {
    let mut feature = FeedbackFeature {
        visible: true,
        draft: Some(draft()),
        ..Default::default()
    };
    let make = || {
        screenshots::encode(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255])),
        ))
        .unwrap()
    };
    feature.paste_images();
    feature.send();
    assert!(
        feature.task.take_pending().is_none(),
        "wait for screenshots before freezing the payload"
    );
    let first = feature.image_task.take_pending().unwrap();
    feature.complete_images(TaskResult {
        id: first.id,
        result: Ok(vec![make(), make(), make()]),
    });
    assert_eq!(feature.draft.as_ref().unwrap().payload.screenshots.len(), 3);
    feature.paste_images();
    let extra = feature.image_task.take_pending().unwrap();
    feature.complete_images(TaskResult {
        id: extra.id,
        result: Ok(vec![make()]),
    });
    assert_eq!(feature.image_error.as_deref(), Some("limit"));
    assert_eq!(feature.draft.as_ref().unwrap().payload.screenshots.len(), 3);
    feature.image_task.begin(screenshots::Source::Clipboard);
    let old = feature.image_task.take_pending().unwrap();
    feature.open(OpenContext::shelf(&egui::Context::default()));
    feature.complete_images(TaskResult {
        id: old.id,
        result: Ok(vec![make()]),
    });
    assert!(feature.prepared_images.is_empty());
}

#[test]
fn stale_completion_cannot_replace_an_active_draft_or_reopen_the_dialog() {
    let mut feature = FeedbackFeature {
        draft: Some(draft()),
        ..Default::default()
    };
    feature.send();
    let pending = feature.task.take_pending().unwrap();
    feature.complete(TaskResult {
        id: pending.id + 1,
        result: Err("storage".into()),
    });
    assert!(feature.task.is_pending());
    assert!(!feature.is_open());
    assert!(feature.locked);
}

#[test]
fn api_responses_must_match_the_request_and_expected_github_repository() {
    let id = "report-id";
    let response = serde_json::json!({"requestId":id,"status":"submitted","issue":{"number":123,"url":"https://github.com/TortoTech/torto/issues/123"}});
    let bytes = serde_json::to_vec(&response).unwrap();
    assert!(matches!(
        transport::parse_response(201, &bytes, id, None),
        Outcome::Submitted(_)
    ));
    assert!(matches!(
        transport::parse_response(201, &bytes, "other", None),
        Outcome::Failed(Failure::Protocol, _)
    ));
    let mut wrong = response;
    wrong["issue"]["url"] = "https://evil.example".into();
    assert!(matches!(
        transport::parse_response(201, &serde_json::to_vec(&wrong).unwrap(), id, None),
        Outcome::Failed(Failure::Protocol, _)
    ));
    let pending =
        serde_json::to_vec(&serde_json::json!({"requestId":id,"status":"pending"})).unwrap();
    assert!(
        matches!(transport::parse_response(202, &pending, id, Some(Duration::from_secs(60))), Outcome::Pending(wait) if wait.as_secs()==60)
    );
    assert!(
        matches!(transport::parse_response(429, b"{}", id, Some(Duration::from_secs(120))), Outcome::Failed(Failure::RateLimited, Some(wait)) if wait.as_secs()==120)
    );
}

#[test]
fn endpoint_and_metadata_never_include_credentials_or_absolute_book_paths() {
    assert!(transport::endpoint(transport::DEFAULT_ENDPOINT).is_ok());
    assert!(transport::endpoint("http://[::1]/api/feedback").is_ok());
    for url in [
        "http://public.example/api/feedback",
        "https://user:secret@example.com/api/feedback",
        "https://example.com/api/feedback?token=secret",
    ] {
        assert!(transport::endpoint(url).is_err());
    }
    for path in [
        "C:\\Users\\Chris\\book.epub",
        "/Users/chris/book.epub",
        "../private",
        "https://private.example",
    ] {
        assert!(book_document(path).is_none());
    }
    let value = serde_json::to_value(draft().payload.context).unwrap();
    assert!(value.get("book").is_none());
    assert!(value.get("location").is_none());
    assert!(value.get("reader").is_none());
    let mut stored = serde_json::to_value(draft()).unwrap();
    stored["payload"]["context"]["apiKey"] = "secret".into();
    assert!(serde_json::from_value::<Draft>(stored).is_err());
}

#[test]
fn persisted_submission_survives_restart_without_new_context_or_request_id() {
    let original = draft();
    let folder = std::env::temp_dir().join(format!("torto-feedback-{}", uuid::Uuid::new_v4()));
    let path = folder.join("draft.json");
    save_draft(&path, &original).unwrap();
    let restored: Draft = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&original.payload).unwrap(),
        serde_json::to_value(&restored.payload).unwrap()
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn focus_locations_bound_member_count_and_bytes_without_shortening_source_ids() {
    use rebook_publication::{SourceAnchor, SourceRange, SpineItemId};
    let start = SourceAnchor {
        spine: SpineItemId::new("chapter").unwrap(),
        node: "paragraph".into(),
        text_offset: 3,
    };
    let range = SourceRange {
        end: SourceAnchor {
            text_offset: 11,
            ..start.clone()
        },
        start,
    };
    let members = vec![range.clone(); 100];
    let focus = FocusLocation::from_source(&range, &members).unwrap();
    assert_eq!(focus.ranges.len(), 64);
    assert_eq!(focus.total_ranges, 100);
    let mut large = range.clone();
    large.start.node = "&".repeat(500);
    large.end.node = large.start.node.clone();
    let focus = FocusLocation::from_source(&large, &vec![large.clone(); 64]).unwrap();
    assert!(focus.ranges.len() < 64);
    assert_eq!(focus.range.start.node, large.start.node);
    assert_eq!(focus.total_ranges, 64);
    assert!(
        serde_json::to_string(&focus)
            .unwrap()
            .replace('&', "&amp;")
            .len()
            <= 16_000
    );
    assert!(FocusLocation::from_source(&range, &[]).is_some());
    large.start.node.push('x');
    assert!(FocusLocation::from_source(&large, &[]).is_none());
}

#[test]
fn focus_location_round_trips_while_legacy_drafts_keep_their_exact_wire_shape() {
    let original = draft();
    let wire = serde_json::to_value(&original).unwrap();
    let restored: Draft = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), wire);
    let old_location = serde_json::json!({ "document": "chapter.xhtml", "anchor": "heading" });
    let restored: Location = serde_json::from_value(old_location.clone()).unwrap();
    assert!(restored.focus.is_none());
    assert_eq!(serde_json::to_value(restored).unwrap(), old_location);
}

#[test]
fn feedback_context_displays_focus_separately_from_chapter_anchor_and_handles_missing_focus() {
    let ctx = egui::Context::default();
    let mut context = OpenContext::shelf(&ctx).context;
    context.location = Some(serde_json::from_value(serde_json::json!({
        "document": "chapter.xhtml", "anchor": "chapter-heading",
        "focus": {
            "range": { "start": { "spine": "chapter", "node": "paragraph-8", "textOffset": 4 },
                       "end": { "spine": "chapter", "node": "paragraph-8", "textOffset": 19 } },
            "ranges": [{ "start": { "spine": "chapter", "node": "paragraph-8", "textOffset": 4 },
                         "end": { "spine": "chapter", "node": "paragraph-8", "textOffset": 19 } }],
            "totalRanges": 2
        }
    })).unwrap());
    context.reader = Some(Reader {
        mode: "focus".into(),
        typesetting: "unified".into(),
        pdf_reflow: None,
        translation: Translation {
            enabled: false,
            target_language: "zh-CN".into(),
        },
    });
    let render = |context: &model::Context| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 1600.0),
                )),
                ..Default::default()
            },
            |ui| show_context(ui, context, AppLanguage::English),
        );
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    render(&context);
    let text = render(&context);
    for expected in [
        "Chapter anchor",
        "chapter-heading",
        "Focused block",
        "paragraph-8",
        "Focus source range",
        "1/2 (truncated)",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    let mut report = draft();
    report.payload.context = context.clone();
    let wire = serde_json::to_value(&report).unwrap();
    let restored: Draft = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), wire);
    context.location.as_mut().unwrap().focus = None;
    assert!(render(&context).contains("Focus location unavailable"));
    context.reader.as_mut().unwrap().mode = "classic".into();
    assert!(!render(&context).contains("Focused block"));
}

#[test]
fn closing_and_reopening_preserves_a_pending_report_and_its_cooldown() {
    let mut feature = FeedbackFeature {
        draft: Some(draft()),
        ..Default::default()
    };
    feature.send();
    let pending = feature.task.take_pending().unwrap();
    let Work::Send(mut sent) = pending.payload else {
        panic!()
    };
    sent.retry_until = Some(now_seconds() + 120);
    feature.visible = false;
    feature.complete(TaskResult {
        id: pending.id,
        result: Ok(Output::Sent(
            sent.clone(),
            Outcome::Pending(Duration::from_secs(120)),
        )),
    });
    assert!(!feature.is_open());
    let mut new_context = OpenContext::shelf(&egui::Context::default());
    new_context.context.app.version = "changed".into();
    feature.open(new_context);
    assert!(feature.is_open());
    assert_eq!(
        feature.draft.as_ref().unwrap().payload.request_id,
        sent.payload.request_id
    );
    assert_ne!(
        feature.draft.as_ref().unwrap().payload.context.app.version,
        "changed"
    );
    feature.send();
    assert!(feature.task.take_pending().is_none());
    feature.retry_at = Some(Instant::now() - Duration::from_secs(1));
    feature.send();
    assert!(feature.task.take_pending().is_some());
}

#[test]
fn http_transport_sends_only_the_report_and_honors_retry_after() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut report = draft();
    report.endpoint = format!("http://{address}/api/feedback");
    let expected = serde_json::to_value(&report.payload).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("feedback connection: {e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let (headers, body) = loop {
            let mut chunk = [0; 4096];
            let size = socket.read(&mut chunk).unwrap();
            assert!(size > 0);
            request.extend_from_slice(&chunk[..size]);
            assert!(request.len() < 70_000);
            if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break (headers, request[end + 4..end + 4 + length].to_vec());
                }
            }
        };
        assert!(headers.starts_with("post /api/feedback http/1.1"));
        assert!(headers.contains("content-type: application/json"));
        for header in ["authorization:", "cookie:", "origin:"] {
            assert!(!headers.lines().any(|line| line.starts_with(header)));
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            expected
        );
        socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 2\r\nRetry-After: 60\r\nConnection: close\r\n\r\n{}").unwrap();
    });
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(transport::submit(&report));
    server.join().unwrap();
    assert!(
        matches!(result, Outcome::Failed(Failure::RateLimited, Some(wait)) if wait.as_secs() == 60)
    );
}

#[test]
fn reopening_an_unsent_draft_refreshes_context_but_preserves_the_description() {
    let original = draft();
    let mut feature = FeedbackFeature {
        loaded: true,
        draft: Some(original.clone()),
        ..Default::default()
    };
    let mut context = OpenContext::shelf(&egui::Context::default());
    context.context.app.version = "new-context".into();
    feature.open(context.clone());
    let pending = feature.task.take_pending().unwrap();
    let Work::Prepare(seed, restore) = pending.payload else {
        panic!()
    };
    assert!(!restore);
    assert_eq!(seed.context.app.version, "new-context");
    let mut prepared = original.clone();
    prepared.payload.description.clear();
    prepared.payload.context = context.context;
    feature.complete(TaskResult {
        id: pending.id,
        result: Ok(Output::Ready(prepared, false)),
    });
    let ready = feature.draft.unwrap();
    assert_eq!(ready.payload.description, original.payload.description);
    assert_eq!(ready.payload.context.app.version, "new-context");
    assert!(!feature.locked);
}
