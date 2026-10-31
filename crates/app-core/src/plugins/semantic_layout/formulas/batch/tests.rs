use super::*;

#[test]
fn replacement_fidelity_is_required_and_unsafe_text_or_latex_is_terminal() {
    let item = json!({"image_id":0,"status":"recognized_text","text":"t = 0 sec",
        "literal":true,"latex":null,"equation_number":null,"table_html":null});
    assert!(parse(&json!({"results":[item.clone()]}).to_string(), &[0]).is_empty());
    let mut unsafe_text = item;
    unsafe_text["replacement_safe"] = json!(false);
    let unsafe_math = json!({"image_id":1,"status":"recognized","replacement_safe":false,
        "text":null,"literal":true,"latex":"\\text{t = 0 sec}","equation_number":null,"table_html":null});
    let value = json!({"results":[unsafe_text,unsafe_math]});
    let schema = options();
    assert!(
        jsonschema::validator_for(&schema["output_schema"])
            .unwrap()
            .is_valid(&value)
    );
    let results = parse(&value.to_string(), &[0, 1]);
    assert_eq!(results.len(), 2);
    for response in results.values() {
        assert!(!response.transient);
        assert!(response.formula().unwrap().is_none());
        let mut annotations = Vec::new();
        let mut failed = 0;
        record(
            Some(response),
            &PublicationUrl::parse("image.png").unwrap(),
            &mut annotations,
            &mut failed,
        );
        assert!(annotations.is_empty());
        assert_eq!(failed, 0);
    }
}

fn complete_image_response(mut value: Value) -> Value {
    if let Some(items) = value["results"].as_array_mut() {
        for item in items {
            if let Some(object) = item.as_object_mut() {
                object
                    .entry("replacement_safe")
                    .or_insert(Value::Bool(true));
                object.entry("text").or_insert(Value::Null);
                object.entry("table_html").or_insert(Value::Null);
                object.entry("literal").or_insert(Value::Bool(false));
            }
        }
    }
    value
}

#[test]
fn mixed_math_and_unicode_text_results_keep_ids_and_character_examples() {
    let value = r#"{"results":[{"image_id":2,"status":"recognized_text","replacement_safe":true,"text":"か","literal":true,"latex":null,"equation_number":null},{"image_id":0,"status":"recognized","replacement_safe":true,"text":null,"literal":false,"latex":"x^2","equation_number":null},{"image_id":1,"status":"not_formula","replacement_safe":false,"text":null,"literal":false,"latex":null,"equation_number":null}]}"#;
    let results = parse(value, &[0, 1, 2]);
    assert_eq!(results.len(), 3);
    assert_eq!(results[&2].text.as_deref(), Some("か"));
    assert!(results[&2].literal);
    assert!(results[&2].formula().unwrap().is_none());
    assert!(results[&0].formula().unwrap().is_some());
    let damaged = r#"{"results":[{"image_id":2,"status":"recognized_text","replacement_safe":true,"text":"\ufffd","literal":false,"latex":null,"equation_number":null}]}"#;
    let results = parse(damaged, &[2]);
    assert_eq!(results[&2].status, "unreadable_text");
    assert!(results[&2].transient);
}

#[test]
fn batch_results_match_ids_and_isolate_bad_items() {
    let item = json!({"image_id":0,"status":"recognized","replacement_safe":true,"latex":"x=1","equation_number":null});
    assert_eq!(
        parse(&json!({"results":[item.clone(),item]}).to_string(), &[0]).len(),
        1
    );
    let result = parse(
        r#"{"results":[
        {"image_id":2,"status":"not_formula","replacement_safe":false,"latex":null,"equation_number":null},
        {"image_id":0,"status":"recognized","replacement_safe":true,"latex":"x=1","equation_number":null},
        {"image_id":1,"status":"recognized","replacement_safe":true,"latex":"$bad$","equation_number":null}
    ]}"#,
        &[0, 1, 2],
    );
    assert_eq!(result.len(), 3);
    assert_eq!(result[&1].status, "unreadable");
    assert!(result[&1].transient);
    assert!(result[&1].latex.is_none());
    assert_eq!(result[&0].latex.as_deref(), Some("x=1"));
    assert_eq!(result[&2].status, "not_formula");
    let duplicate = parse(
        r#"{"results":[
        {"image_id":0,"status":"recognized","replacement_safe":true,"latex":"x=1","equation_number":null},
        {"image_id":0,"status":"recognized","replacement_safe":true,"latex":"x=2","equation_number":null},
        {"image_id":99,"status":"not_formula","replacement_safe":false,"latex":null,"equation_number":null}
    ]}"#,
        &[0, 1],
    );
    assert!(duplicate.is_empty());
}

#[test]
fn batch_retries_only_missing_items_without_reviewing_valid_siblings() {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
            let mut size = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    size = n.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; size];
            reader.read_exact(&mut bytes).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["reasoning_effort"], "high");
            let input = body["messages"][1]["content"].as_array().unwrap();
            let header: Value = serde_json::from_str(input[0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(
                header["ri"],
                match turn {
                    0 => json!([0, 1, 2]),
                    1 => json!([1]),
                    _ => json!([0, 1]),
                }
            );
            assert!(header.get("m").is_none());
            assert_eq!(
                input.iter().filter(|v| v["type"] == "image_url").count(),
                match turn {
                    0 => 3,
                    1 => 1,
                    _ => 4,
                }
            );
            assert_eq!(
                body["response_format"]["json_schema"]["schema"]["required"],
                json!(["r"])
            );
            let instructions = body["messages"][0]["content"].to_string();
            assert!(!instructions.contains("Compact JSON transport"));
            assert!(instructions.contains("ri=requested_ids") && !instructions.contains("m=mode"));
            assert!(!instructions.contains("g=groups") && !instructions.contains("pr=proposal"));
            let status = &body["response_format"]["json_schema"]["schema"]["properties"]["r"]["items"]
                ["properties"]["ss"];
            assert_eq!(status["enum"], json!(["ok", "tx", "no", "u", "ut", "tb"]));
            for meaning in [
                "ok=recognized",
                "tx=recognized_text",
                "no=not_formula",
                "u=unreadable",
                "ut=unreadable_text",
                "tb=recognized_table",
            ] {
                assert!(status["description"].as_str().unwrap().contains(meaning));
            }
            let item = |id, latex: Option<&str>| json!({"image_id":id,"status":if latex.is_some(){"recognized"}else{"not_formula"},"latex":latex,"equation_number":null});
            let results = match turn {
                0 => vec![item(2, None), item(0, Some("x=1"))],
                1 => vec![item(1, Some("y=2"))],
                _ => vec![item(1, Some("y=2")), item(0, Some("x=1"))],
            };
            let response =
                json!({"choices":[{"index":0,"finish_reason":"stop","message":{"content":super::super::super::wire::encode(&complete_image_response(json!({"results":results}))).to_string()}}]})
                    .to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
        }
    });
    let provider = crate::plugins::AiProvider {
        kind: crate::plugins::AiProviderKind::OpenAi,
        base_url: format!("http://{address}/v1"),
        api_key: "fixture-key".into(),
        ..Default::default()
    };
    let batch: Vec<_> = (0..3)
        .map(|i| Pending {
            key: i.to_string(),
            path: None,
            aliases: vec![],
            url: "data:image/png;base64,fixture".into(),
            candidate: Candidate {
                image: ImageBlock {
                    ocr_text: None,
                    formula_image: false,
                    formula: None,
                    href: PublicationUrl::parse(&format!("math-{i}.png")).unwrap(),
                    alt: String::new(),
                    style: Default::default(),
                    source: None,
                    text_layer: None,
                },
                inline: false,
                context: String::new(),
            },
        })
        .collect();
    let client = crate::http::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(request_batch(
            &client,
            &provider,
            "fixture",
            ReasoningEffort::High,
            &batch,
        ))
        .unwrap();
    server.join().unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].as_ref().unwrap().latex.as_deref(), Some("x=1"));
    assert_eq!(result[1].as_ref().unwrap().latex.as_deref(), Some("y=2"));
    assert_eq!(result[2].as_ref().unwrap().status, "not_formula");
    assert!(result.iter().all(|r| !r.as_ref().unwrap().transient));
}

fn run_batch_fixture(replies: Vec<(Value, Vec<usize>)>) -> Vec<Option<Response>> {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for (reply, ids) in replies {
            let started = std::time::Instant::now();
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(socket) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(started.elapsed() < Duration::from_secs(5));
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["reasoning_effort"], "high");
            let schema = &request["response_format"]["json_schema"]["schema"];
            assert_eq!(schema["required"], json!(["r"]));
            assert!(schema["properties"].get("latex").is_none());
            let fields = &schema["properties"]["r"]["items"]["properties"];
            for field in ["ii", "ss", "l", "n"] {
                assert!(
                    fields[field]["description"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
            }
            assert_eq!(request["messages"][0]["role"], "system");
            let prompt = request["messages"][0]["content"].to_string();
            assert!(!prompt.contains("Conditional review"));
            assert!(!prompt.contains("proposal") && !prompt.contains("local_validation_error"));
            assert!(prompt.contains("Transcription self-check"));
            let content = request["messages"][1]["content"].as_array().unwrap();
            let header: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
            assert!(header.get("m").is_none());
            assert_eq!(header["ri"], json!(ids));
            assert_eq!(
                content.iter().filter(|v| v["type"] == "image_url").count(),
                ids.len()
            );
            let (status, body) = if reply.is_null() {
                ("500 Internal Server Error", "{}".to_owned())
            } else {
                (
                    "200 OK",
                    json!({"choices":[{"index":0,"finish_reason":"stop","message":{"content":super::super::super::wire::encode(&complete_image_response(reply)).to_string()}}]}).to_string(),
                )
            };
            write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let provider = crate::plugins::AiProvider {
        kind: crate::plugins::AiProviderKind::OpenAi,
        base_url: format!("http://{address}/v1"),
        api_key: "fixture".into(),
        ..Default::default()
    };
    let batch: Vec<_> = (0..2)
        .map(|i| Pending {
            key: i.to_string(),
            path: None,
            aliases: vec![],
            url: "data:image/png;base64,fixture".into(),
            candidate: Candidate {
                image: ImageBlock {
                    ocr_text: None,
                    formula_image: false,
                    formula: None,
                    href: PublicationUrl::parse(&format!("math-{i}.png")).unwrap(),
                    alt: String::new(),
                    style: Default::default(),
                    source: None,
                    text_layer: None,
                },
                inline: false,
                context: String::new(),
            },
        })
        .collect();
    let client = crate::http::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(request_batch(
            &client,
            &provider,
            "fixture",
            ReasoningEffort::High,
            &batch,
        ))
        .unwrap();
    server.join().unwrap();
    result
}

fn recognized(id: usize, latex: &str) -> Value {
    json!({"image_id":id,"status":"recognized","replacement_safe":true,"latex":latex,"equation_number":null})
}

#[test]
fn valid_batch_finishes_after_one_request() {
    let result = run_batch_fixture(vec![(
        json!({"results":[recognized(0,"x=1"),recognized(1,"y=2")]}),
        vec![0, 1],
    )]);
    assert!(
        result
            .iter()
            .all(|item| item.as_ref().unwrap().formula().unwrap().is_some())
    );
}

#[test]
fn invalid_formula_keeps_original_without_a_review_request() {
    let result = run_batch_fixture(vec![(
        json!({"results":[recognized(0,"x=1"),recognized(1,r"\frac{")]}),
        vec![0, 1],
    )]);
    assert_eq!(result[0].as_ref().unwrap().latex.as_deref(), Some("x=1"));
    let invalid = result[1].as_ref().unwrap();
    assert_eq!(invalid.status, "unreadable");
    assert!(invalid.transient);
    assert!(invalid.latex.is_none());
    assert!(invalid.formula().unwrap().is_none());
}
