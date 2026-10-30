use super::super::tests::{original_source, section, text};
use super::*;

fn table() -> Section {
    let mut table=rebook_html::parse_recognized_table("<table><caption>增长率</caption><tr><th>行业</th><th>%</th></tr><tr><td>A</td><td>-4</td></tr><tr><td>B</td><td>0</td></tr><tr><td>C</td><td>4</td></tr></table>").unwrap();
    table.source = source(&text("table", "table")).cloned();
    for (r, row) in table.rows.iter_mut().enumerate() {
        for (c, cell) in row.cells.iter_mut().enumerate() {
            cell.text.source = source(&text(
                &format!("cell-{r}-{c}"),
                &text_block_text(&cell.text),
            ))
            .cloned();
        }
    }
    section(vec![Block::Table(table)])
}
fn chart() -> Chart {
    serde_json::from_value(json!({"tables":[0],"kind":"bar","title":"行业比较","panels":[{"title":"增长率","unit":"%","categories":["甲","乙","丙"],"series":[{"name":"增长率","cells":[[0,1,1],[0,2,1],[0,3,1]]}]}]})).unwrap()
}
fn formula(id: &str, latex: &str, display: bool) -> Block {
    let Block::Text(mut t) = text(id, "") else {
        unreachable!()
    };
    t.content = vec![Inline::Math(rebook_publication::MathRun {
        latex: latex.into(),
        original: None,
        display,
        size_scale: 1.0,
    })];
    if !display {
        t.content.insert(
            0,
            Inline::Text(rebook_publication::TextRun {
                text: "Inline prose ".into(),
                style: Default::default(),
                link: None,
            }),
        );
    }
    Block::Text(t)
}

#[test]
fn chart_values_are_source_backed_and_negative_bars_use_zero_baseline() {
    let raw = table();
    let (tables, formulas) = candidates(&raw);
    let annotations = validate(
        Output {
            charts: vec![chart()],
            notes: vec![],
        },
        &tables,
        &formulas,
        "简体中文",
    );
    let Annotation::TableChart { svg, language, .. } = &annotations[0] else {
        panic!()
    };
    assert_eq!(language, "简体中文");
    let rects = regex::Regex::new(
        "<rect x='[^']+' y='([^']+)' width='[^']+' height='([^']+)' fill='#39886b'",
    )
    .unwrap();
    let bars = rects
        .captures_iter(svg)
        .take(3)
        .map(|c| (c[1].parse::<f64>().unwrap(), c[2].parse::<f64>().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(bars.len(), 3);
    assert!(bars[0].1 > 0.0 && bars[2].1 > 0.0);
    assert_eq!(bars[1].1, 0.0, "zero must not become a positive bar");
    assert!(
        (bars[0].0 - bars[1].0).abs() < 0.01,
        "negative bar starts at zero"
    );
    assert!(
        (bars[2].0 + bars[2].1 - bars[1].0).abs() < 0.01,
        "positive bar ends at zero"
    );
    assert!(resvg::usvg::Tree::from_data(svg.as_bytes(), &Default::default()).is_ok());
}

#[test]
fn invented_cells_missing_values_and_invalid_pies_are_rejected() {
    let raw = table();
    let (tables, formulas) = candidates(&raw);
    for (kind, cell) in [("bar", [0, 999, 1]), ("bar", [0, 1, 0]), ("pie", [0, 1, 1])] {
        let mut plan = chart();
        plan.kind = kind.into();
        plan.panels[0].series[0].cells[0] = cell;
        assert!(
            validate(
                Output {
                    charts: vec![plan],
                    notes: vec![]
                },
                &tables,
                &formulas,
                "French"
            )
            .is_empty()
        );
    }
    assert_eq!(numeric("−3.9%"), Some(-3.9));
    assert_eq!(numeric("1,234.5"), Some(1234.5));
    for missing in ["—", "N/A", "", "<5", "12 ± 1", "NaN", "inf"] {
        assert!(numeric(missing).is_none());
    }
}

#[test]
fn notes_require_formula_symbols_and_exact_local_evidence_and_exclude_inline_math() {
    let raw = section(vec![
        formula("display", "T/S", true),
        text("definition", "T is the final string. S is seconds."),
        formula("inline", "T/S", false),
    ]);
    let (tables, formulas) = candidates(&raw);
    assert_eq!(formulas.len(), 1);
    let make = |symbol: &str, meaning: &str, evidence: &str| SymbolNote {
        symbol: symbol.into(),
        meaning: meaning.into(),
        unit: String::new(),
        evidence: evidence.into(),
        known: true,
    };
    let notes = vec![
        make("T", "最终字符串", "T is the final string."),
        make("S", "温度", "unrelated evidence"),
        make("Q", "未知", "S is seconds."),
    ];
    let result = validate(
        Output {
            charts: vec![],
            notes: vec![Notes { id: 0, notes }],
        },
        &tables,
        &formulas,
        "简体中文",
    );
    let Annotation::FormulaNotes {
        notes, language, ..
    } = &result[0]
    else {
        panic!()
    };
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].meaning, "最终字符串");
    assert_eq!(language, "简体中文");
    assert_eq!(
        input(&tables, &formulas, "French")["target_language"],
        "French"
    );
}

#[test]
fn short_wire_keys_preserve_book_text_and_schema_roundtrips() {
    let value = json!({"target_language":"French","charts":[{"title":"notes","panels":[]}],"notes":[{"id":0,"notes":[{"symbol":"T","meaning":"string","unit":"","evidence":"T is string","known":true}]}]});
    let encoded = wire::encode(&value);
    assert_eq!(encoded["tl"], "French");
    assert!(encoded.get("notes").is_none());
    assert!(encoded.get("charts").is_none());
    assert_eq!(wire::decode(&encoded), value);
    assert!(
        wire::options(&schema())["output_schema"]["properties"]
            .get("nt")
            .is_some()
    );
}

#[test]
fn chart_toggle_keeps_table_cells_source_identity_and_immutable_resource() {
    let raw = table();
    let (tables, formulas) = candidates(&raw);
    let annotations = validate(
        Output {
            charts: vec![chart()],
            notes: vec![],
        },
        &tables,
        &formulas,
        "简体中文",
    );
    let mut result = empty_recognition(&raw);
    result.annotations = annotations;
    let original = original_source(raw.clone());
    let source = Arc::new(SemanticLayoutSource::new(original.clone(), original));
    source.state.write().unwrap().insert(0, result);
    let Block::Table(raw_table) = &raw.blocks[0] else {
        panic!()
    };
    let origin = raw_table.source.as_ref().unwrap();
    let first = source.parse_section(0).unwrap();
    let Block::Table(first) = &first.blocks[0] else {
        panic!()
    };
    assert!(!first.visualization.as_ref().unwrap().active);
    let mut session = rebook_reader::ReaderSession::open_with_fonts(
        source.clone(),
        rebook_layout::LayoutViewport::new(800, 600).unwrap(),
        rebook_layout::ReaderStyle {
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            spread: rebook_layout::SpreadMode::Scroll,
            ..Default::default()
        },
        crate::fonts::embedded_reader_fonts(),
    )
    .unwrap();
    assert!(source.toggle_chart(origin));
    let anchor = source.chart_anchor(origin).unwrap();
    session = session
        .prepare_refresh_request(Some(anchor.clone()))
        .prepare()
        .unwrap();
    assert!(session.restore_cached_anchor(&anchor.start));
    let second = source.parse_section(0).unwrap();
    let Block::Table(second) = &second.blocks[0] else {
        panic!()
    };
    assert_eq!(first.rows, second.rows);
    assert_eq!(first.source, second.source);
    assert!(second.visualization.as_ref().unwrap().active);
    let href = second
        .visualization
        .as_ref()
        .unwrap()
        .href
        .as_ref()
        .unwrap();
    assert_eq!(
        &*source.resource(href).unwrap().bytes,
        &*source.resource(href).unwrap().bytes
    );
    assert_eq!(
        source.image_resource_dimensions(href).unwrap().unwrap()[0],
        960
    );
    let pixels = source.raster_resource(href).unwrap().unwrap();
    assert_eq!(
        pixels.pixels.len(),
        pixels.width as usize * pixels.height as usize * 4
    );
    assert!(
        pixels
            .pixels
            .chunks_exact(4)
            .filter(|p| p[0] < 90 && p[1] < 120 && p[2] < 100 && p[3] > 200)
            .count()
            > 300,
        "chart text and axes must materialize through the production resource path"
    );
    if let Some(dir) = std::env::var_os("TORTO_FEATURE_ARTIFACTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        image::save_buffer(
            dir.join("table-chart.png"),
            &pixels.pixels,
            pixels.width,
            pixels.height,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    assert!(source.toggle_chart(origin));
    let anchor = source.chart_anchor(origin).unwrap();
    session = session
        .prepare_refresh_request(Some(anchor.clone()))
        .prepare()
        .unwrap();
    assert!(session.restore_cached_anchor(&anchor.start));
    assert!(!source.chart_control(origin).unwrap().1);
}

#[test]
fn layout_request_uses_short_keys_target_language_and_reuses_only_matching_cache() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for language in ["French", "日本語"] {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..at]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                    if bytes.len() >= at + 4 + length {
                        let request: Value =
                            serde_json::from_slice(&bytes[at + 4..at + 4 + length]).unwrap();
                        let content = request["messages"][1]["content"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                request["messages"][1]["content"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .filter_map(|v| v["text"].as_str())
                                    .collect()
                            });
                        let input: Value = serde_json::from_str(&content).unwrap();
                        assert_eq!(input["tl"], language);
                        assert!(input.get("tables").is_none());
                        assert!(input.get("tb").is_some());
                        break;
                    }
                }
            }
            let mut plan = chart();
            plan.title = if language == "French" {
                "Comparaison"
            } else {
                "業種比較"
            }
            .into();
            let result = serde_json::to_value(Output {
                charts: vec![plan],
                notes: vec![],
            })
            .unwrap();
            let body=json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":wire::encode(&result).to_string()}}]}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let provider = crate::plugins::AiProvider {
        kind: crate::plugins::AiProviderKind::OpenAi,
        base_url: format!("http://{address}/v1"),
        api_key: "test-key".into(),
        models: vec![crate::plugins::AiModelConfig::language("layout-test")],
        ..Default::default()
    };
    let mut settings = PluginSettings::default();
    settings.semantic_layout.enabled = true;
    settings.semantic_layout.provider = provider.id.clone();
    settings.semantic_layout.model = "layout-test".into();
    settings.providers = vec![provider];
    let raw = table();
    let targets = raw.blocks.iter().flat_map(block_ranges).collect::<Vec<_>>();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = crate::http::builder().no_proxy().build().unwrap();
    let book = format!("enhancements-test-{address}");
    for language in ["French", "日本語", "日本語"] {
        settings.target_language = language.into();
        let (annotations, ok) =
            runtime.block_on(recognize(&client, &settings, &raw, &targets, &book));
        assert!(ok, "layout request and cache must be usable");
        assert_eq!(annotations.len(), 1);
        assert!(matches!(&annotations[0],Annotation::TableChart{language:l,..} if l==language));
    }
    server.join().unwrap();
}

#[test]
#[ignore = "Requires the two issue books in the local library; no model calls"]
fn local_issue_books_supply_continued_table_sources_and_formula_definitions() {
    let library =
        PathBuf::from("/Users/lchris/Library/Application Support/com.Rebook.Rebook/library/books");
    let opened = rebook_formats::open_file(
        library.join("bd03d1c8ff3345bee37da96ad50c04095b7e47502789d07cc45cb3d4803ad60a.epub"),
    )
    .unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().ends_with("chapter2_0001.xhtml"))
        .unwrap();
    let mut raw = source.parse_section(index).unwrap();
    let images = formulas::candidates(&raw);
    let first = images
        .iter()
        .find(|i| i.image.href.path().ends_with("image_002.jpg"))
        .unwrap();
    let second = images
        .iter()
        .find(|i| i.image.href.path().ends_with("image_003.jpg"))
        .unwrap();
    let mut a=rebook_html::parse_recognized_table("<table><tr><th>产业</th><th>2003年</th><th>2004年</th><th>增长率</th></tr><tr><td>全产业平均</td><td>198.1</td><td>195.0</td><td>-1.6</td></tr><tr><td>建设业</td><td>196.6</td><td>193.2</td><td>-1.7</td></tr><tr><td>制造业</td><td>199.2</td><td>197.6</td><td>-0.8</td></tr></table>").unwrap();
    let mut b=rebook_html::parse_recognized_table("<table><caption>续表</caption><tr><th>产业</th><th>2003年</th><th>2004年</th><th>增长率</th></tr><tr><td>运输业</td><td>189.8</td><td>190.2</td><td>0.2</td></tr></table>").unwrap();
    a.source = first.image.source.clone();
    b.source = second.image.source.clone();
    image_tables::compose(&mut raw.blocks, &first.image.href, &a);
    image_tables::compose(&mut raw.blocks, &second.image.href, &b);
    let (tables, _) = candidates(&raw);
    assert_eq!(tables.len(), 2);
    let chart=serde_json::from_value(json!({"tables":[0,1],"kind":"bar","title":"起薪","panels":[{"title":"对比","unit":"千日元","categories":["建设业","制造业","运输业"],"series":[{"name":"2004","cells":[[0,2,2],[0,3,2],[1,1,2]]}]}]})).unwrap();
    let annotations = validate(
        Output {
            charts: vec![chart],
            notes: vec![],
        },
        &tables,
        &[],
        "简体中文",
    );
    assert_eq!(
        annotations.len(),
        1,
        "explicit continuation must share one chart"
    );
    let Annotation::TableChart {
        source: root,
        members,
        svg,
        ..
    } = &annotations[0]
    else {
        panic!()
    };
    apply_chart(&mut raw.blocks, root, members, svg);
    let (tables, _) = candidates(&raw);
    assert!(
        tables[0]
            .table
            .visualization
            .as_ref()
            .unwrap()
            .href
            .is_some()
    );
    assert!(
        tables[1]
            .table
            .visualization
            .as_ref()
            .unwrap()
            .href
            .is_none()
    );

    let opened = rebook_formats::open_file(
        library.join("1147c97489ff38024c5de930f9c9bd2201cda04caaba28f92e1d5030eb45eee5.epub"),
    )
    .unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().ends_with("B9780123735911500036.htm"))
        .unwrap();
    let mut raw = source.parse_section(index).unwrap();
    let candidates = formulas::candidates(&raw);
    let equation = candidates
        .iter()
        .find(|i| i.image.href.path().ends_with("_si1.gif"))
        .unwrap();
    formulas::compose(
        &mut raw.blocks,
        &equation.image.href,
        Some(&rebook_publication::ImageFormula {
            latex: "WPM=\\frac{|T|-1}{S}\\cdot60\\cdot\\frac{1}{5}".into(),
            equation_number: None,
        }),
    );
    let (_, found) = super::candidates(&raw);
    let wpm = found
        .iter()
        .find(|f| f.latex.starts_with("WPM"))
        .expect("display image formula needs notes");
    assert!(wpm.context.contains("T is the final transcribed string"));
    assert!(wpm.context.contains("S term is seconds"));
    assert!(wpm.context.contains("seconds per minute"));
}
