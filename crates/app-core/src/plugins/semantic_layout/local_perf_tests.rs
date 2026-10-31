//! Explicit local-library diagnostic; never runs in CI or writes reading state.
use super::*;
use std::time::Instant;

#[test]
#[ignore = "requires TORTO_PERF_BOOKS and TORTO_PERF_PROBE_ROOT; local library audit"]
fn local_semantic_phase_probe() {
    let books: Value =
        serde_json::from_slice(&std::fs::read(std::env::var("TORTO_PERF_BOOKS").unwrap()).unwrap())
            .unwrap();
    let root = PathBuf::from(std::env::var("TORTO_PERF_PROBE_ROOT").unwrap());
    let library = PathBuf::from("C:/Users/Chris/AppData/Local/Rebook/Rebook/data/library/books");
    let settings = super::super::PluginSettings::load_default().unwrap();
    let mut output = Vec::new();
    for (book, index) in books.as_array().unwrap().iter().zip([20, 10, 16]) {
        let mut samples = Vec::new();
        for _ in 0..5 {
            let publication = rebook_formats::open_file_for_reading_cached(
                &library.join(book["storage_name"].as_str().unwrap()),
                Some(book["id"].as_str().unwrap()),
                &root,
            )
            .unwrap();
            let original = Arc::new(super::super::RewriteBookSource::new(publication.source()));
            let translation = Arc::new(super::super::TranslationBookSource::new(
                original.clone(),
                settings.translation_mode,
            ));
            let source = SemanticLayoutSource::new(translation.clone(), original.clone());
            source.configure(
                &super::super::pdf_native::cache_identity(
                    book["id"].as_str().unwrap(),
                    original.as_ref(),
                ),
                &settings,
            );
            source.set_unified_citations(true);
            let mut phases = serde_json::Map::new();
            macro_rules! timed {
                ($name:literal, $action:expr) => {{
                    let started = Instant::now();
                    let value = $action;
                    phases.insert(
                        $name.into(),
                        (started.elapsed().as_secs_f64() * 1000.0).into(),
                    );
                    value
                }};
            }
            let mut section = timed!("inner_parse_ms", translation.parse_section(index).unwrap());
            let (canonical, hash) = timed!("snapshot_ms", source.original_snapshot(index).unwrap());
            assert_eq!(
                hash,
                fingerprint(&canonical),
                "persisted fingerprints must preserve existing AI recognition keys"
            );
            let identity = source.cache_identity.read().unwrap().clone();
            let recognition = timed!(
                "ai_cache_ms",
                identity.as_ref().and_then(
                    |identity| source.cached_recognition(index, &canonical, &hash, identity)
                )
            );
            phases.insert(
                "ai_annotations".into(),
                recognition
                    .as_ref()
                    .map_or(0, |r| r.annotations.len())
                    .into(),
            );
            timed!("ai_compose_ms", {
                if let Some(recognition) = recognition {
                    for annotation in recognition.annotations {
                        if headings::annotation_eligible(&canonical, &annotation) {
                            compose(&mut section.blocks, &annotation);
                        }
                    }
                }
                text_formulas::normalize_formula_quotes(&mut section.blocks);
            });
            let annotations = timed!(
                "heuristics_ms",
                source.cached_heuristics(index, &canonical, &hash)
            );
            phases.insert("heuristic_annotations".into(), annotations.len().into());
            timed!("citation_compose_ms", {
                citations::compose_many(&mut section.blocks, &annotations);
            });
            timed!(
                "fallback_ms",
                citations::apply_heuristic_fallback(&mut section)
            );
            let mut paragraphs = Vec::new();
            fn texts<'a>(blocks: &'a [Block], result: &mut Vec<&'a TextBlock>) {
                for block in blocks {
                    match block {
                        Block::Text(text) => result.push(text),
                        Block::Quote(quote) => {
                            result.extend(quote.body.iter().chain(quote.attribution.iter()))
                        }
                        Block::Figure(figure) => result.extend(figure.text_blocks()),
                        Block::Table(table) => result.extend(table.text_blocks()),
                        Block::Note(note) => texts(&note.blocks, result),
                        _ => {}
                    }
                }
            }
            texts(&canonical.blocks, &mut paragraphs);
            phases.insert(
                "text_characters".into(),
                paragraphs
                    .iter()
                    .map(|t| text_block_text(t).chars().count())
                    .sum::<usize>()
                    .into(),
            );
            phases.insert(
                "text_runs".into(),
                paragraphs
                    .iter()
                    .map(|t| t.content.len())
                    .sum::<usize>()
                    .into(),
            );
            phases.insert("paragraphs".into(), paragraphs.len().into());
            phases.insert("blocks".into(), canonical.blocks.len().into());
            samples.push(phases);
        }
        output.push(json!({"book":book["title"], "profile":"dev", "samples":samples}));
    }
    std::fs::write(
        root.join("semantic-phases.json"),
        serde_json::to_vec_pretty(&output).unwrap(),
    )
    .unwrap();
    println!(
        "SEMANTIC_PHASES {}",
        serde_json::to_string(&output).unwrap()
    );
}
