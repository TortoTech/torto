use super::*;

#[test]
fn batch_results_match_ids_and_isolate_bad_items() {
    let result = parse(
        r#"{"results":[
        {"image_id":2,"status":"not_formula","latex":null,"equation_number":null},
        {"image_id":0,"status":"recognized","latex":"x=1","equation_number":null},
        {"image_id":1,"status":"recognized","latex":"$bad$","equation_number":null}
    ]}"#,
        &[0, 1, 2],
    );
    assert_eq!(result.len(), 2);
    assert_eq!(result[&0].latex.as_deref(), Some("x=1"));
    assert_eq!(result[&2].status, "not_formula");
    let duplicate = parse(
        r#"{"results":[
        {"image_id":0,"status":"recognized","latex":"x=1","equation_number":null},
        {"image_id":0,"status":"recognized","latex":"x=2","equation_number":null},
        {"image_id":99,"status":"not_formula","latex":null,"equation_number":null}
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
            let input = body["messages"][1]["content"].as_array().unwrap();
            let header: Value = serde_json::from_str(input[0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(
                header["requested_ids"],
                match turn {
                    0 => json!([0, 1, 2]),
                    1 => json!([1]),
                    _ => json!([0, 1]),
                }
            );
            assert_eq!(
                header["mode"],
                if turn == 2 { "verify" } else { "transcribe" }
            );
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
                json!(["results"])
            );
            let item = |id, latex: Option<&str>| json!({"image_id":id,"status":if latex.is_some(){"recognized"}else{"not_formula"},"latex":latex,"equation_number":null});
            let results = match turn {
                0 => vec![item(2, None), item(0, Some("x=1"))],
                1 => vec![item(1, Some("y=2"))],
                _ => vec![item(1, Some("y=2")), item(0, Some("x=1"))],
            };
            let response =
                json!({"choices":[{"message":{"content":json!({"results":results}).to_string()}}]})
                    .to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
        }
    });
    let provider = crate::plugins::AiProvider {
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(request_batch(&client, &provider, "fixture", &batch))
        .unwrap();
    server.join().unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].as_ref().unwrap().latex.as_deref(), Some("x=1"));
    assert_eq!(result[1].as_ref().unwrap().latex.as_deref(), Some("y=2"));
    assert_eq!(result[2].as_ref().unwrap().status, "not_formula");
    assert!(result.iter().all(|r| !r.as_ref().unwrap().transient));
}

fn run_review_fixture(replies: Vec<(Value, &'static str, Vec<usize>)>) -> Vec<Option<Response>> {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for (reply, mode, ids) in replies {
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
            let content = request["messages"][1]["content"].as_array().unwrap();
            let header: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(header["mode"], mode);
            assert_eq!(header["requested_ids"], json!(ids));
            assert!(
                request["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("Transcription self-check")
            );
            let (status, body) = if reply.is_null() {
                ("500 Internal Server Error", "{}".to_owned())
            } else {
                (
                    "200 OK",
                    json!({"choices":[{"message":{"content":reply.to_string()}}]}).to_string(),
                )
            };
            write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let provider = crate::plugins::AiProvider {
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(request_batch(&client, &provider, "fixture", &batch))
        .unwrap();
    server.join().unwrap();
    result
}

fn recognized(id: usize, latex: &str) -> Value {
    json!({"image_id":id,"status":"recognized","latex":latex,"equation_number":null})
}

#[test]
fn valid_batch_finishes_after_one_request() {
    let result = run_review_fixture(vec![(
        json!({"results":[recognized(0,"x=1"),recognized(1,"y=2")]}),
        "transcribe",
        vec![0, 1],
    )]);
    assert!(
        result
            .iter()
            .all(|item| item.as_ref().unwrap().formula().unwrap().is_some())
    );
}

#[test]
fn only_local_validation_failures_are_reviewed_and_corrected() {
    let result = run_review_fixture(vec![
        (
            json!({"results":[recognized(0,"x=1"),recognized(1,r"\frac{")]}),
            "transcribe",
            vec![0, 1],
        ),
        (
            json!({"results":[recognized(1,r"\frac{a}{b}")]}),
            "verify",
            vec![1],
        ),
    ]);
    assert_eq!(result[0].as_ref().unwrap().latex.as_deref(), Some("x=1"));
    assert_eq!(
        result[1].as_ref().unwrap().latex.as_deref(),
        Some(r"\frac{a}{b}")
    );
    assert!(!result[1].as_ref().unwrap().transient);
}

#[test]
fn failed_review_keeps_original_only_for_the_bad_image() {
    let result = run_review_fixture(vec![
        (
            json!({"results":[recognized(0,"x=1"),recognized(1,r"\frac{")]}),
            "transcribe",
            vec![0, 1],
        ),
        (Value::Null, "verify", vec![1]),
    ]);
    assert_eq!(result[0].as_ref().unwrap().status, "recognized");
    assert_eq!(result[1].as_ref().unwrap().status, "unreadable");
    assert!(result[1].as_ref().unwrap().transient);
}

#[test]
fn review_rendering_respects_source_and_nesting_limits() {
    let mut response = Response {
        status: "recognized".into(),
        latex: Some("{".repeat(33) + "x" + &"}".repeat(33)),
        equation_number: None,
        transient: false,
    };
    assert_eq!(
        rendered_url(&response).unwrap_err(),
        "Formula nesting limit"
    );
    response.latex = None;
    assert_eq!(rendered_url(&response).unwrap_err(), "Missing LaTeX");
}
