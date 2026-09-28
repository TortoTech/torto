use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn schema() -> Value {
    json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false})
}

#[test]
fn legacy_provider_settings_keep_gateway_and_default_output_policy() {
    let mut settings = super::super::PluginSettings::default();
    settings.providers[0] = serde_json::from_value(json!({
        "id":"legacy", "kind":"open-ai", "name":"Gateway",
        "base_url":"https://gateway.example/v1", "models":[{"id":"test"}],
        "structured_output":"prompt", "allow_output_tools":false
    }))
    .unwrap();
    settings.normalize();
    let provider = &settings.providers[0];
    assert_eq!(provider.base_url, "https://gateway.example/v1");
    assert_eq!(provider.id, "legacy");
    assert_eq!(provider.structured_output, OutputMode::Auto);
    assert!(provider.allow_output_tools);
    let saved = serde_json::to_value(provider).unwrap();
    assert!(saved.get("structured_output").is_none());
    assert!(saved.get("allow_output_tools").is_none());
    assert!(AiProviderKind::Anthropic.matches_search("claude"));
    assert!(AiProviderKind::Moonshot.matches_search("kimi"));
}
fn wire_message(content: Value) -> Value {
    json!({"id":"test","object":"chat.completion","created":0,"model":"test","choices":[{"index":0,"message":content,"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})
}
fn text_response(text: &str) -> Value {
    wire_message(json!({"role":"assistant","content":text}))
}
fn tool_response(value: Value) -> Value {
    wire_message(
        json!({"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":OUTPUT_TOOL,"arguments":value.to_string()}}]}),
    )
}

pub(crate) fn server(responses: Vec<(u16, Value)>) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, response) in responses {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("missing expected HTTP request: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                    }
                }
            };
            requests.push(body);
            let body = response.to_string();
            write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    (url, handle)
}

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
async fn ask(provider: &AiProvider, schema: Value) -> Result<Value, String> {
    complete(
        provider,
        "test",
        &[
            json!({"role":"system","content":"Follow the task rules"}),
            json!({"role":"system","content":[{"type":"text","text":"Additional rules"},{"type":"text","text":"Keep source data unchanged"}]}),
            json!({"role":"user","content":"Return the result"}),
        ],
        None,
        None,
        ReasoningEffort::Default,
        Some(&schema_options(schema)),
    )
    .await
}

#[test]
fn four_modes_have_identical_results_and_distinct_wire_contracts() {
    for mode in [
        OutputMode::Native,
        OutputMode::Tool,
        OutputMode::JsonObject,
        OutputMode::Prompt,
    ] {
        let response = if mode == OutputMode::Tool {
            tool_response(json!({"ok":true}))
        } else {
            text_response("{\"ok\":true}")
        };
        let (url, handle) = server(vec![(200, response)]);
        let provider = AiProvider {
            base_url: url,
            structured_output: mode,
            ..Default::default()
        };
        let result = run(ask(&provider, schema())).unwrap();
        assert_eq!(result["_structured_value"], json!({"ok":true}));
        let requests = handle.join().unwrap();
        let request = &requests[0];
        let messages = request["messages"].as_array().unwrap();
        for instruction in [
            "Follow the task rules",
            "Additional rules",
            "Keep source data unchanged",
        ] {
            let matching: Vec<_> = messages
                .iter()
                .filter(|m| m["content"].to_string().contains(instruction))
                .collect();
            assert_eq!(matching.len(), 1);
            assert_eq!(matching[0]["role"], "system");
        }
        assert_eq!(messages.last().unwrap()["role"], "user");
        match mode {
            OutputMode::Native => assert_eq!(request["response_format"]["type"], "json_schema"),
            OutputMode::Tool => {
                assert_eq!(request["tools"][0]["function"]["name"], OUTPUT_TOOL);
                assert!(request.get("response_format").is_none());
            }
            OutputMode::JsonObject => {
                assert_eq!(request["response_format"]["type"], "json_object");
                assert!(
                    request["messages"][0]["content"]
                        .to_string()
                        .contains("additionalProperties")
                );
            }
            OutputMode::Prompt => {
                assert!(request.get("response_format").is_none());
                assert!(request.get("tools").is_none());
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn unsupported_modes_fall_back_once_then_are_cached() {
    let error = |field: &str| json!({"error":{"message":format!("{field} is not supported")}});
    let (url, handle) = server(vec![
        (400, error("response_format json_schema")),
        (400, error("tool_choice")),
        (400, error("response_format")),
        (200, text_response("{\"ok\":true}")),
        (200, text_response("{\"ok\":true}")),
    ]);
    let provider = AiProvider {
        kind: AiProviderKind::OpenAi,
        base_url: url,
        ..Default::default()
    };
    run(async {
        assert_eq!(ask(&provider, schema()).await.unwrap()["_llm_attempts"], 4);
        assert_eq!(ask(&provider, schema()).await.unwrap()["_llm_attempts"], 1);
    });
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests[4].get("response_format").is_none());
    assert!(requests[4].get("tools").is_none());
}

#[test]
fn custom_output_defaults_to_prompt_and_deepseek_effort_is_preserved() {
    assert_eq!(
        modes(&AiProvider::default(), &schema(), false),
        vec![OutputMode::Prompt]
    );
    for (effort, wire) in [
        (ReasoningEffort::Low, "low"),
        (ReasoningEffort::High, "high"),
        (ReasoningEffort::Max, "max"),
    ] {
        let params = reasoning_params(AiProviderKind::DeepSeek, "deepseek-flash", effort);
        assert_eq!(params["thinking"]["type"], "enabled");
        assert_eq!(params["reasoning_effort"], wire);
    }
    let off = reasoning_params(
        AiProviderKind::DeepSeek,
        "deepseek-flash",
        ReasoningEffort::None,
    );
    assert_eq!(off["thinking"]["type"], "disabled");
    assert!(off.get("reasoning_effort").is_none());
}

#[test]
fn malformed_structured_tool_arguments_are_repaired_before_rig_deserialization() {
    let mut response = tool_response(json!({"ok":true}));
    response["choices"][0]["message"]["content"] = json!("");
    response["choices"][0]["finish_reason"] = json!("tool_calls");
    response["choices"][0]["message"]["tool_calls"][0]["index"] = json!(0);
    response["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
        json!("{\"ok\":true}}");
    let (url, handle) = server(vec![(200, response)]);
    let provider = AiProvider {
        base_url: url,
        kind: AiProviderKind::DeepSeek,
        ..Default::default()
    };
    assert_eq!(
        run(ask(&provider, schema())).unwrap()["_structured_value"],
        json!({"ok":true})
    );
    assert_eq!(handle.join().unwrap().len(), 1);
}

#[test]
fn output_tool_wraps_array_and_does_not_run_a_second_turn() {
    let (url, handle) = server(vec![(200, tool_response(json!({"result":[1,2]})))]);
    let provider = AiProvider {
        base_url: url,
        structured_output: OutputMode::Tool,
        ..Default::default()
    };
    assert_eq!(
        run(ask(
            &provider,
            json!({"type":"array","items":{"type":"integer"}})
        ))
        .unwrap()["_structured_value"],
        json!([1, 2])
    );
    assert_eq!(handle.join().unwrap().len(), 1);
}

#[test]
fn native_and_json_object_modes_unwrap_non_object_schemas() {
    for mode in [OutputMode::Native, OutputMode::JsonObject] {
        let (url, handle) = server(vec![(200, text_response("{\"result\":[1,2]}"))]);
        let provider = AiProvider {
            base_url: url,
            structured_output: mode,
            ..Default::default()
        };
        assert_eq!(
            run(ask(
                &provider,
                json!({"type":"array","items":{"type":"integer"}})
            ))
            .unwrap()["_structured_value"],
            json!([1, 2])
        );
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

#[test]
fn invalid_json_schema_and_invalid_results_are_not_capability_failures() {
    let provider = AiProvider::default();
    assert!(
        run(ask(&provider, json!({"type":"invalid"})))
            .unwrap_err()
            .contains("Schema")
    );
    let (url, handle) = server(vec![(200, text_response("{\"ok\":\"not a boolean\"}"))]);
    let provider = AiProvider {
        base_url: url,
        ..Default::default()
    };
    assert!(
        run(ask(&provider, schema()))
            .unwrap_err()
            .contains("Schema 校验失败")
    );
    assert_eq!(handle.join().unwrap().len(), 1);
}

#[test]
fn only_explicit_capability_errors_allow_fallback() {
    for status in [401, 403, 429, 500] {
        let error = CompletionError::from_http_response(
            reqwest_rig::StatusCode::from_u16(status).unwrap(),
            r#"{"error":{"message":"response_format unsupported"}}"#,
        );
        assert!(!can_fallback(&error));
    }
    let error = CompletionError::from_http_response(
        reqwest_rig::StatusCode::BAD_REQUEST,
        r#"{"error":{"message":"invalid schema: required is missing"}}"#,
    );
    assert!(!can_fallback(&error));
}

#[test]
fn capability_policy_skips_disabled_tools_and_preserves_schema_constraints() {
    let provider = AiProvider {
        kind: AiProviderKind::DeepSeek,
        allow_output_tools: false,
        ..Default::default()
    };
    assert_eq!(
        modes(&provider, &schema(), false),
        vec![OutputMode::JsonObject, OutputMode::Prompt]
    );
    assert!(!native_schema_compatible(
        &json!({"type":"object","patternProperties":{".*":{"type":"string"}}}),
        AiProviderKind::OpenAi
    ));
}

#[test]
fn xai_responses_output_and_reasoning_use_native_parameter_names() {
    let provider = AiProvider {
        kind: AiProviderKind::Xai,
        ..Default::default()
    };
    let (request, _) = build_request(
        &provider,
        "grok-test",
        &[json!({"role":"user","content":"result"})],
        None,
        None,
        ReasoningEffort::High,
        None,
    )
    .unwrap();
    let (native, _) = prepare_output(
        request.clone(),
        &schema(),
        OutputMode::Native,
        provider.kind,
    )
    .unwrap();
    let params = native.additional_params.unwrap();
    assert_eq!(params["text"]["format"]["schema"], schema());
    assert_eq!(params["reasoning"]["effort"], "high");
    assert!(native.output_schema.is_none());
    let (object, _) =
        prepare_output(request, &schema(), OutputMode::JsonObject, provider.kind).unwrap();
    assert_eq!(
        object.additional_params.unwrap()["text"]["format"]["type"],
        "json_object"
    );
}

#[test]
fn manual_native_mode_never_silently_drops_schema() {
    let provider = AiProvider {
        kind: AiProviderKind::DeepSeek,
        structured_output: OutputMode::Native,
        ..Default::default()
    };
    assert!(
        run(ask(&provider, schema()))
            .unwrap_err()
            .contains("不支持 Native")
    );
}

#[test]
fn task_budget_is_shared_across_corrections_and_fallbacks() {
    run(budgeted(async {
        for _ in 0..4 {
            consume_attempt().unwrap();
        }
        assert!(
            budgeted(async { consume_attempt().map_err(|e| e.to_string()) })
                .await
                .is_err()
        );
        Ok(())
    }))
    .unwrap();
}

#[test]
fn tool_history_keeps_names_ids_and_reasoning_signatures() {
    use rig_core::message::{Reasoning, ToolCall, ToolFunction};
    let call = ToolCall::from_wire(
        "call-7",
        ToolFunction {
            name: "read_book".into(),
            arguments: json!({"page":1}),
        },
    );
    let response = CompletionResponse::new(
        vec![
            AssistantContent::Reasoning(Reasoning::new_with_signature(
                "thinking",
                Some("signed".into()),
            )),
            AssistantContent::ToolCall(call),
        ],
        Default::default(),
        "anthropic",
    );
    let history = messages_from_legacy(&[
        response_message(&response),
        json!({"role":"tool","tool_call_id":"call-7","content":"book text"}),
    ])
    .unwrap();
    assert!(matches!(&history[0],Message::Assistant{content,..} if content==&response.choice));
    assert!(
        matches!(&history[1],Message::User{content} if matches!(&content[0],UserContent::ToolResult(result) if result.name=="read_book"))
    );
}
