use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

#[test]
fn hosted_search_preserves_book_functions_on_compatible_endpoints() {
    for kind in [AiProviderKind::OpenRouter, AiProviderKind::Zai] {
        let (url, handle) = server(vec![(200, text_response("answer"))]);
        let provider = AiProvider {
            kind,
            base_url: url,
            ..Default::default()
        };
        let params = super::super::web_search::native_params(&provider, "test").unwrap();
        let tools = json!([{"type":"function","function":{"name":"searchBook","description":"Search book","parameters":{"type":"object","properties":{}}}}]);
        let result = run(complete(
            &provider,
            "test",
            &[json!({"role":"user","content":"search"})],
            Some(&tools),
            None,
            ReasoningEffort::Default,
            Some(&params),
        ));
        assert!(result.is_ok(), "{kind:?}: {result:?}");
        let requests = handle.join().unwrap();
        let tools = requests[0]["tools"].as_array().unwrap();
        assert!(
            tools.iter().any(|t| t["function"]["name"] == "searchBook"),
            "{kind:?}: {tools:?}"
        );
        assert!(
            tools.iter().any(|t| t["type"] != "function"),
            "{kind:?}: {tools:?}"
        );
    }
}

fn schema() -> Value {
    json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false})
}

#[test]
fn native_search_wire_keeps_functions_and_internal_flags_private() {
    for (kind, model) in [
        (AiProviderKind::OpenAi, "gpt-5"),
        (AiProviderKind::OpenAi, "gpt-5-2025-08-07"),
        (AiProviderKind::Anthropic, "claude-sonnet-4-6"),
        (AiProviderKind::Gemini, "gemini-3-pro"),
        (AiProviderKind::Xai, "grok-4"),
    ] {
        let (url, handle) = server(vec![(
            400,
            json!({"error":{"message":"fixture rejects request"}}),
        )]);
        let provider = AiProvider {
            kind,
            base_url: url,
            ..Default::default()
        };
        let params = super::super::web_search::native_params(&provider, model).unwrap();
        let tools = json!([{"type":"function","function":{"name":"searchBook","description":"Search book","parameters":{"type":"object","properties":{}}}}]);
        if kind == AiProviderKind::Custom {
            let _ = run(stream_with_search(
                &provider,
                model,
                &[json!({"role":"user","content":"search"})],
                Some(&tools),
                ReasoningEffort::Default,
                Some(&params),
                &mut |_| {},
            ));
        } else {
            let _ = run(complete(
                &provider,
                model,
                &[json!({"role":"user","content":"search"})],
                Some(&tools),
                None,
                ReasoningEffort::Default,
                Some(&params),
            ));
        }
        let requests = handle.join().unwrap();
        let body = &requests[0];
        assert!(body.to_string().contains("searchBook"), "{kind:?}: {body}");
        assert!(
            body.to_string().contains(if model.starts_with("gemini-") {
                "googleSearch"
            } else {
                "web_search"
            }),
            "{kind:?}: {body}"
        );
        assert!(body.get("_torto_responses").is_none());
        if kind == AiProviderKind::OpenAi {
            assert!(body.get("temperature").is_none());
        }
        if kind == AiProviderKind::Custom {
            assert_eq!(body["_fixture_path"], "/v1/chat/completions");
            assert!(
                body["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["function"]["name"] == "searchBook")
            );
        }
    }
}

#[test]
fn custom_gemini_search_keeps_chat_completions_protocol() {
    let model = "cpa/gemini-3.8-flash-high";
    let (url, handle) = server(vec![(200, text_response("answer"))]);
    let provider = AiProvider {
        kind: AiProviderKind::Custom,
        base_url: url,
        ..Default::default()
    };
    let params = super::super::web_search::native_params(&provider, model);
    let tools = json!([{"type":"function","function":{"name":"searchBook","description":"Search book","parameters":{"type":"object","properties":{}}}}]);
    let message = run(stream_with_search(
        &provider,
        model,
        &[json!({"role":"user","content":"search"})],
        Some(&tools),
        ReasoningEffort::Default,
        params.as_ref(),
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(message["content"], "answer");
    let requests = handle.join().unwrap();
    assert_eq!(requests[0]["_fixture_path"], "/v1/chat/completions");
    assert_eq!(requests[0]["model"], model);
    let tools = requests[0]["tools"].as_array().unwrap();
    assert!(params.is_none());
    assert!(tools.iter().all(|t| t["type"] == "function"));
    assert!(tools.iter().any(|t| t["function"]["name"] == "searchBook"));
}

#[test]
fn streamed_native_annotations_are_preserved_as_clickable_sources() {
    let mut response = text_response("A sourced answer");
    response["choices"][0]["message"]["annotations"] = json!([{"type":"url_citation","url_citation":{"url":"https://example.com/evidence","title":"Evidence","start_index":0,"end_index":16}}]);
    let (url, handle) = server(vec![(200, response)]);
    let provider = AiProvider {
        kind: AiProviderKind::OpenRouter,
        base_url: url,
        ..Default::default()
    };
    let params = super::super::web_search::native_params(&provider, "test").unwrap();
    let message = run(stream_with_search(
        &provider,
        "test",
        &[json!({"role":"user","content":"search"})],
        None,
        ReasoningEffort::Default,
        Some(&params),
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(
        message["_web_sources"][0]["url"],
        "https://example.com/evidence"
    );
    handle.join().unwrap();
}

#[test]
fn best_effort_metadata_never_relaxes_primary_translation_validation() {
    let wire = json!({"type":"object","additionalProperties":false,
        "properties":{"0":{"type":"string","minLength":1},"glossary":{"type":"array"}},
        "required":["0","glossary"]});
    let options = json!({"best_effort_output_fields":["glossary"]});
    let local = best_effort_validation_schema(&wire, Some(&options));
    let validator = jsonschema::validator_for(&local).unwrap();
    assert!(validator.is_valid(&json!({"0":"译文"})));
    assert!(validator.is_valid(&json!({"0":"译文","glossary":"bad metadata"})));
    assert!(!validator.is_valid(&json!({"glossary":[]})));
    assert!(!validator.is_valid(&json!({"0":"","glossary":[]})));
    assert!(!validator.is_valid(&json!({"0":12,"glossary":[]})));
    assert_eq!(wire["required"], json!(["0", "glossary"]));
    assert_eq!(best_effort_validation_schema(&wire, None), wire);
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
    server_payloads(
        responses
            .into_iter()
            .map(|(status, value)| (status, "application/json", value.to_string()))
            .collect(),
    )
}

fn server_payloads(
    responses: Vec<(u16, &'static str, String)>,
) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, content_type, body) in responses {
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
            let request = loop {
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
                        let mut body: Value =
                            serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                        body["_fixture_path"] = json!(
                            headers
                                .lines()
                                .next()
                                .unwrap()
                                .split_whitespace()
                                .nth(1)
                                .unwrap()
                        );
                        break body;
                    }
                }
            };
            requests.push(request);
            write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
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
        assert_eq!(
            messages
                .iter()
                .filter(|message| message["role"] == "system")
                .count(),
            1
        );
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
        if matches!(mode, OutputMode::Prompt | OutputMode::JsonObject) {
            let system = messages[0]["content"].to_string();
            assert!(
                system.find("Follow the task rules").unwrap()
                    < system.find("JSON Schema:").unwrap()
            );
        }
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
fn malformed_structured_tool_arguments_are_repaired_before_validation() {
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
            reqwest::StatusCode::from_u16(status).unwrap(),
            r#"{"error":{"message":"response_format unsupported"}}"#,
        );
        assert!(!can_fallback(&error));
    }
    let error = CompletionError::from_http_response(
        reqwest::StatusCode::BAD_REQUEST,
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
    assert!(params.get("text").is_none());
    assert_eq!(params["reasoning"]["effort"], "high");
    assert_eq!(
        serde_json::to_value(native.output_schema).unwrap(),
        schema()
    );
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
    use rig_core::message::{Origin, ToolCall, ToolFunction, ToolName};
    let provider = AiProvider {
        kind: AiProviderKind::Anthropic,
        ..Default::default()
    };
    let call = ToolCall::from_wire(
        "call-7",
        ToolFunction::new(ToolName::new("read_book").unwrap(), json!({"page":1})),
    );
    let response = CompletionResponse::new(
        vec![
            AssistantContent::reasoning("thinking")
                .with_native(json!({"type":"thinking","thinking":"thinking","signature":"signed"})),
            AssistantContent::ToolCall(call),
        ],
        Default::default(),
        Origin::new("anthropic.messages", "anthropic", "test"),
        json!({"_torto_llm_scope":history::scope(&provider, "test")}),
    );
    let history = history::messages(
        &provider,
        "test",
        &[
            response_message(&response),
            json!({"role":"tool","tool_call_id":"call-7","content":"book text"}),
        ],
    )
    .unwrap();
    assert!(matches!(&history[0],Message::Assistant(turn) if turn.content==response.choice));
    assert!(
        matches!(&history[1],Message::User{content} if matches!(&content[0],UserContent::ToolResult(result) if result.name=="read_book"))
    );
}

fn signed_gateway_message(id: &str) -> Value {
    json!({"role":"assistant","content":null,"reasoning_content":"checking","reasoning":"checking",
        "reasoning_details":[
            {"type":"reasoning.encrypted","id":id,"index":0,"signature":"opaque-signed-value"},
            {"type":"future.gateway.trace","payload":{"keep":true}},
            {"type":"reasoning.encrypted","id":"cipher","index":1,"data":"opaque-ciphertext"}
        ],
        "tool_calls":[{"id":id,"type":"function","function":{"name":"read_draft","arguments":"{\"offset\":0}"},
            "extra_content":{"google":{"thought_signature":"call-signature"}},"gateway_extension":{"keep":true}}]})
}

fn gateway_tools() -> Value {
    json!([{"type":"function","function":{"name":"read_draft","description":"Read draft","parameters":{"type":"object","properties":{"offset":{"type":"integer"}}}}}])
}

#[test]
fn compatible_gateway_signed_history_preserves_opaque_fields_across_tool_rounds() {
    let first = signed_gateway_message("call-1");
    let second = signed_gateway_message("call-2");
    let (url, handle) = server(vec![
        (200, wire_message(first.clone())),
        (200, wire_message(second.clone())),
        (200, text_response("done")),
    ]);
    let provider = AiProvider {
        kind: AiProviderKind::Custom,
        base_url: url,
        ..Default::default()
    };
    let tools = gateway_tools();
    let mut history = vec![json!({"role":"user","content":"Inspect draft"})];
    for id in ["call-1", "call-2"] {
        let message = run(complete(
            &provider,
            "gemini/lite",
            &history,
            Some(&tools),
            None,
            ReasoningEffort::Default,
            None,
        ))
        .unwrap();
        assert_eq!(message["tool_calls"][0]["id"], id);
        history.push(message);
        history.push(json!({"role":"tool","tool_call_id":id,"content":"draft"}));
    }
    run(complete(
        &provider,
        "gemini/lite",
        &history,
        Some(&tools),
        None,
        ReasoningEffort::Default,
        None,
    ))
    .unwrap();
    let requests = handle.join().unwrap();
    for (replayed, original) in [
        (&requests[1]["messages"][1], &first),
        (&requests[2]["messages"][1], &first),
        (&requests[2]["messages"][3], &second),
    ] {
        let mut expected = original.clone();
        // Rig consolidates duplicate reasoning aliases and omits null content.
        expected.as_object_mut().unwrap().remove("reasoning");
        expected.as_object_mut().unwrap().remove("content");
        assert_eq!(replayed, &expected);
    }
    assert_eq!(requests[2]["messages"][2]["tool_call_id"], "call-1");
    assert_eq!(requests[2]["messages"][4]["tool_call_id"], "call-2");
    for request in requests {
        assert!(!request.to_string().contains("_torto_"));
        assert!(!request.to_string().contains("_compatible_message"));
    }
}

#[test]
fn compatible_gateway_history_is_scoped_and_invalidated_by_core_edits() {
    let original = signed_gateway_message("call-1");
    let (url, handle) = server(vec![(200, wire_message(original))]);
    let provider = AiProvider {
        kind: AiProviderKind::Custom,
        base_url: url,
        api_key: "first-key".into(),
        ..Default::default()
    };
    let tools = gateway_tools();
    let message = run(complete(
        &provider,
        "gemini/lite",
        &[json!({"role":"user","content":"inspect"})],
        Some(&tools),
        None,
        ReasoningEffort::Default,
        None,
    ))
    .unwrap();
    handle.join().unwrap();
    let intact = history::messages(&provider, "gemini/lite", &[message.clone()]).unwrap();
    assert!(
        matches!(&intact[0], Message::Assistant(turn) if turn.content.iter().any(|p| p.native_item().is_some()))
    );
    for (changed, model) in [
        (provider.clone(), "other-model"),
        (
            AiProvider {
                base_url: "http://other/v1".into(),
                ..provider.clone()
            },
            "gemini/lite",
        ),
        (
            AiProvider {
                api_key: "second-key".into(),
                ..provider.clone()
            },
            "gemini/lite",
        ),
        (
            AiProvider {
                kind: AiProviderKind::Gemini,
                ..provider.clone()
            },
            "gemini/lite",
        ),
    ] {
        let projected = history::messages(&changed, model, &[message.clone()]).unwrap();
        assert!(
            matches!(&projected[0], Message::Assistant(turn) if turn.content.iter().all(|p| p.native_item().is_none()) && turn.origin.is_none())
        );
    }
    assert!(!message.to_string().contains("first-key"));
    let mut edited = message;
    let mut core: Message = serde_json::from_value(edited["_rig_message"].clone()).unwrap();
    if let Message::Assistant(turn) = &mut core {
        for part in &mut turn.content {
            if let AssistantContent::ToolCall(call) = part {
                call.function.arguments = json!({"offset":9}).as_object().unwrap().clone();
            }
        }
    }
    edited["_rig_message"] = serde_json::to_value(core).unwrap();
    let projected = history::messages(&provider, "gemini/lite", &[edited.clone()]).unwrap();
    assert!(matches!(&projected[0], Message::Assistant(turn)
        if !turn.content.iter().any(|part| matches!(part, AssistantContent::Reasoning(_)))));
    edited["_rig_message"]["content"] = json!([]);
    assert!(
        matches!(&history::messages(&provider, "gemini/lite", &[edited]).unwrap()[0], Message::Assistant(turn) if turn.content.is_empty())
    );
}

#[test]
fn incompatible_signed_history_is_not_sent_to_a_different_model() {
    let (url, handle) = server(vec![
        (200, wire_message(signed_gateway_message("call-1"))),
        (200, text_response("done")),
    ]);
    let provider = AiProvider {
        kind: AiProviderKind::Custom,
        base_url: url,
        ..Default::default()
    };
    let tools = gateway_tools();
    let mut history = vec![json!({"role":"user","content":"inspect"})];
    history.push(
        run(complete(
            &provider,
            "gemini/lite",
            &history,
            Some(&tools),
            None,
            ReasoningEffort::Default,
            None,
        ))
        .unwrap(),
    );
    history.push(json!({"role":"tool","tool_call_id":"call-1","content":"draft"}));
    run(complete(
        &provider,
        "another-model",
        &history,
        Some(&tools),
        None,
        ReasoningEffort::Default,
        None,
    ))
    .unwrap();
    let requests = handle.join().unwrap();
    let echoed = requests[1]["messages"][1].to_string();
    assert!(!echoed.contains("opaque-signed-value"));
    assert!(!echoed.contains("opaque-ciphertext"));
    assert!(!echoed.contains("call-signature"));
    assert!(!echoed.contains("future.gateway.trace"));
}

#[test]
fn malformed_business_calls_and_unfinished_responses_are_rejected() {
    let mut malformed = wire_message(signed_gateway_message("call-1"));
    malformed["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] = json!("{broken");
    let mut unfinished = text_response("partial answer");
    unfinished["choices"][0]
        .as_object_mut()
        .unwrap()
        .remove("finish_reason");
    for (response, expected) in [
        (malformed, "工具参数不是有效 JSON"),
        (unfinished, "未正常完成"),
    ] {
        let (url, handle) = server(vec![(200, response)]);
        let provider = AiProvider {
            base_url: url,
            ..Default::default()
        };
        let result = run(complete(
            &provider,
            "test",
            &[json!({"role":"user","content":"inspect"})],
            Some(&gateway_tools()),
            None,
            ReasoningEffort::Default,
            None,
        ));
        assert!(result.unwrap_err().contains(expected));
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

#[test]
fn genuine_sse_preserves_signed_calls_and_emits_text_once() {
    let mut delta = signed_gateway_message("call-sse");
    delta["tool_calls"][0]["index"] = json!(0);
    let frames = [
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"Hello "},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":"world"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":"tool_calls"}]}),
    ];
    let mut sse = frames
        .iter()
        .map(|f| format!("data: {f}\n\n"))
        .collect::<String>();
    sse.push_str("data: [DONE]\n\n");
    let (url, handle) = server_payloads(vec![
        (200, "text/event-stream", sse),
        (200, "application/json", text_response("done").to_string()),
    ]);
    let provider = AiProvider {
        base_url: url,
        ..Default::default()
    };
    let mut content = Vec::new();
    let tools = gateway_tools();
    let mut history = vec![json!({"role":"user","content":"inspect"})];
    let message = run(stream_with_search(
        &provider,
        "gemini/lite",
        &history,
        Some(&tools),
        ReasoningEffort::Default,
        None,
        &mut |event| {
            if let ChatStreamEvent::Content(text) = event {
                content.push(text);
            }
        },
    ))
    .unwrap();
    assert_eq!(content, ["Hello ", "Hello world"]);
    assert_eq!(message["content"], "Hello world");
    assert_eq!(message["tool_calls"][0]["id"], "call-sse");
    history.push(message);
    history.push(json!({"role":"tool","tool_call_id":"call-sse","content":"draft"}));
    run(complete(
        &provider,
        "gemini/lite",
        &history,
        Some(&tools),
        None,
        ReasoningEffort::Default,
        None,
    ))
    .unwrap();
    let requests = handle.join().unwrap();
    let replay = &requests[1]["messages"][1];
    assert_eq!(replay["reasoning_details"], delta["reasoning_details"]);
    assert_eq!(
        replay["tool_calls"][0]["extra_content"],
        delta["tool_calls"][0]["extra_content"]
    );
    assert_eq!(
        replay["tool_calls"][0]["gateway_extension"],
        delta["tool_calls"][0]["gateway_extension"]
    );
    assert_eq!(requests[1]["messages"][2]["tool_call_id"], "call-sse");
}

#[test]
fn xai_native_schema_and_image_are_encoded_at_the_configured_version_once() {
    let (url, handle) = server(vec![(400, json!({"error":{"message":"fixture"}}))]);
    let provider = AiProvider {
        kind: AiProviderKind::Xai,
        base_url: url,
        structured_output: OutputMode::Native,
        ..Default::default()
    };
    let _ = run(complete(
        &provider,
        "grok-test",
        &[
            json!({"role":"user","content":[{"type":"text","text":"inspect"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aGVsbG8=","detail":"high"}}]}),
        ],
        None,
        None,
        ReasoningEffort::Default,
        Some(&schema_options(schema())),
    ));
    let requests = handle.join().unwrap();
    assert_eq!(requests[0]["_fixture_path"], "/v1/responses");
    assert_eq!(requests[0]["text"]["format"]["type"], "json_schema");
    assert_eq!(requests[0]["text"]["format"]["schema"], schema());
    assert!(
        requests[0]
            .to_string()
            .contains("data:image/png;base64,aGVsbG8=")
    );
}

#[test]
fn ollama_retains_native_chat_and_schema_output() {
    let reply = json!({"model":"test","message":{"role":"assistant","content":"{\"ok\":true}"},"done":true,"done_reason":"stop","prompt_eval_count":1,"eval_count":2});
    let (url, handle) = server(vec![(200, reply)]);
    let provider = AiProvider {
        kind: AiProviderKind::Ollama,
        base_url: url.trim_end_matches("/v1").into(),
        structured_output: OutputMode::Native,
        ..Default::default()
    };
    let result = run(ask(&provider, schema())).unwrap();
    assert_eq!(result["_structured_value"], json!({"ok":true}));
    let requests = handle.join().unwrap();
    assert_eq!(requests[0]["_fixture_path"], "/api/chat");
    assert_eq!(requests[0]["format"], schema());
}
