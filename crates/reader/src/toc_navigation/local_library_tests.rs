use super::*;
/// Optional read-only corpus audit. The manifest is an array of {title,path}.
/// No AI, credentials, personal progress or publication files are written.
#[test]
#[ignore = "requires TORTO_TOC_AUDIT_MANIFEST and optionally TORTO_TOC_AUDIT_OUTPUT"]
fn local_library_preserves_valid_authored_toc_and_bounds_heading_recovery() {
    let manifest = std::fs::read(std::env::var_os("TORTO_TOC_AUDIT_MANIFEST").unwrap()).unwrap();
    let inputs: Vec<serde_json::Value> = serde_json::from_slice(&manifest).unwrap();
    let mut reports = Vec::new();
    let mut failures = Vec::new();
    for input in inputs {
        let started = std::time::Instant::now();
        let path = input["path"].as_str().unwrap();
        let title = input["title"].as_str().unwrap();
        let publication = match rebook_formats::open_file_for_reading(path, None) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{title}: open: {e}"));
                continue;
            }
        };
        let source = publication.source();
        let authored = source.book().clone();
        if authored.metadata.layout == RenditionLayout::PrePaginated {
            reports.push(serde_json::json!({"title":title,"fixed_layout_skipped":true}));
            continue;
        }
        let repository = SectionRepository::new(source.clone());
        let mut recovered = Vec::new();
        let mut valid = 0;
        let mut unresolved = 0;
        let mut parse_errors = Vec::new();
        let mut entries = 0;
        for index in 0..repository.sections.len() {
            let section = match repository.load(index) {
                Ok(s) => s,
                Err(e) => {
                    if source.parse_section(index).is_err() {
                        parse_errors.push(format!("{index}: {e}"));
                    } else {
                        failures.push(format!("{title}, section {index}: {e}"));
                    }
                    continue;
                }
            };
            for row in &repository.navigation[index] {
                entries += 1;
                let original = row.target.as_ref().unwrap();
                let target = &section.toc_targets[&row.id];
                if let Some(fragment) = original.fragment()
                    && let Some(anchor) = section
                        .fragments
                        .iter()
                        .flat_map(|f| &f.anchors)
                        .find(|a| a.fragment == fragment)
                {
                    valid += 1;
                    if target.href != *original || target.source.as_ref() != Some(&anchor.source) {
                        failures.push(format!("{title}: changed valid anchor {}", row.label));
                    }
                }
                if target.href != *original {
                    let anchor = target.source.as_ref().unwrap();
                    assert!(section.source_order.contains_key(&anchor.node));
                    let texts = section
                        .fragments
                        .iter()
                        .flat_map(|f| &f.blocks)
                        .flat_map(Block::text_blocks)
                        .filter(|t| {
                            t.source
                                .as_ref()
                                .is_some_and(|r| source_range_contains(r, anchor))
                        })
                        .collect::<Vec<_>>();
                    assert!(
                        texts
                            .iter()
                            .any(|t| matches!(t.kind, TextBlockKind::Heading(_))
                                || heading_text(t).trim_start().starts_with('©'))
                            || target
                                .href
                                .fragment()
                                .is_some_and(|_| original.fragment().is_none())
                                && section
                                    .fragments
                                    .iter()
                                    .flat_map(|f| &f.anchors)
                                    .any(|a| a.source == *anchor
                                        && a.fragment.to_lowercase().starts_with("copyright")),
                        "{title}: unexpected recovery {}",
                        row.label
                    );
                    recovered.push(serde_json::json!({"label":row.label,"id":row.id,"source":format!("{}/{}",anchor.spine.as_str(),anchor.node)}));
                } else if !target.active {
                    unresolved += 1;
                }
            }
        }
        let rows = flatten_toc(&authored.table_of_contents);
        let mut navigation_pairs = 0;
        if !rows.is_empty() {
            let mut reader = ReaderSession::open_deferred_with_fonts(
                source.clone(),
                LayoutViewport::new(900, 700).unwrap(),
                ReaderStyle::default(),
                Arc::default(),
                None,
            )
            .unwrap();
            let samples = [0, rows.len() / 2, rows.len() - 1]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            for sample in samples {
                let row = &rows[sample];
                let Some(href) = &row.target else { continue };
                if reader.section_index_for_href(href).is_none() {
                    continue;
                }
                reader.go_to_href(href).unwrap();
                let reference = reader.current_position();
                reader.go_to_toc(&row.id).unwrap();
                if !recovered.iter().any(|r| r["id"].as_str() == Some(&row.id)) {
                    assert_eq!(
                        reader.current_position(),
                        reference,
                        "{title}: normal target changed for {}",
                        row.label
                    );
                }
                assert!((0.0..=1.0).contains(&reader.snapshot().total_progression));
                navigation_pairs += 1;
            }
            if title == "The Algorithm Design Manual" {
                for style in [
                    ReaderStyle::default(),
                    ReaderStyle {
                        typesetting: rebook_layout::ReaderTypesetting::unified(),
                        ..Default::default()
                    },
                ] {
                    reader.set_style(style).unwrap();
                    for (id, node) in [("1", "n18"), ("2", "n24"), ("3", "n61")] {
                        reader.go_to_toc(id).unwrap();
                        assert_eq!(reader.source_anchor_for_toc(id).unwrap().node, node);
                        assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some(id));
                    }
                    reader.go_to_toc("2").unwrap();
                    reader
                        .go_to_adjacent_reading_unit(PageDirection::Next)
                        .unwrap();
                    assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("3"));
                    reader
                        .go_to_adjacent_reading_unit(PageDirection::Previous)
                        .unwrap();
                    assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("2"));
                }
            }
        }
        assert_eq!(
            source.book(),
            &authored,
            "audit must not rewrite authored navigation"
        );
        let report = serde_json::json!({"title":title,"sections":repository.sections.len(),"entries":entries,"valid_fragments_preserved":valid,"navigation_pairs":navigation_pairs,"recovered":recovered,"unresolved_duplicates":unresolved,"baseline_parse_errors":parse_errors,"seconds":started.elapsed().as_secs_f64()});
        println!("TOC_AUDIT {report}");
        reports.push(report);
    }
    if let Some(path) = std::env::var_os("TORTO_TOC_AUDIT_OUTPUT") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({"books":reports,"failures":failures}))
                .unwrap(),
        )
        .unwrap();
    }
    assert!(failures.is_empty(), "corpus failures: {failures:?}");
}
