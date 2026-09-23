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
fn batch_retries_only_missing_items_and_verifies_together() {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for turn in 0..3 {
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
