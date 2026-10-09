//! Provider protocols live in Rig. Output negotiation and validation live here,
//! so book operations never depend on a provider's JSON dialect.
mod history;
#[cfg(test)]
pub(super) mod tests;
mod transport;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rig_core::completion::{CompletionRequest, CompletionResponse, FinishReason, ToolDefinition};
use rig_core::driver::Model;
use rig_core::error::ProviderError as CompletionError;
use rig_core::message::{AssistantContent, Message, ToolChoice, ToolResultContent, UserContent};
use rig_core::operation::Completion;
use rig_core::providers;
use rig_core::streaming::{Item, StreamEvent};
use rig_core::wire::{Encoded, Wire, WireFrame};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{AiProvider, AiProviderKind, ReasoningEffort, ai::ChatStreamEvent, llm_json};

const OUTPUT_TOOL: &str = "torto_structured_result";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const CACHE_TTL: Duration = Duration::from_secs(3600);

tokio::task_local! {
    static ATTEMPTS: std::cell::Cell<u8>;
}

/// One budget for capability fallback AND a task's existing correction loop.
pub(super) async fn budgeted<T>(
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    if ATTEMPTS.try_with(|_| ()).is_ok() {
        return future.await;
    }
    ATTEMPTS
        .scope(std::cell::Cell::new(4), async {
            tokio::time::timeout(REQUEST_TIMEOUT, future)
                .await
                .map_err(|_| "AI 任务超时".to_owned())?
        })
        .await
}

fn consume_attempt() -> Result<(), CompletionError> {
    ATTEMPTS
        .try_with(|remaining| {
            if remaining.get() == 0 {
                return Err(CompletionError::Provider(
                    "AI 任务已达到总请求次数上限".into(),
                ));
            }
            remaining.set(remaining.get() - 1);
            Ok(())
        })
        .unwrap_or(Ok(()))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutputMode {
    #[default]
    Auto,
    Native,
    Tool,
    JsonObject,
    Prompt,
}

impl OutputMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Native => "Native JSON Schema",
            Self::Tool => "Tool Calling + Schema",
            Self::JsonObject => "JSON Object + Schema Prompt",
            Self::Prompt => "Prompt-only JSON",
        }
    }
}

// Includes the schema and request features: a rejected schema must never turn
// off native output for all requests to that model. Credentials are hashed.
type CapabilityCache = HashMap<String, (Instant, HashSet<OutputMode>)>;
static CAPABILITIES: OnceLock<Mutex<CapabilityCache>> = OnceLock::new();

fn cache_key(
    provider: &AiProvider,
    model: &str,
    schema: &Value,
    request: &CompletionRequest,
) -> String {
    let mut hash = Sha256::new();
    hash.update(format!(
        "{:?}|{}|{}|{}|{}|{:?}|{}|{}",
        provider.kind,
        provider.id,
        provider.base_url,
        model,
        provider.api_key,
        provider.structured_output,
        provider.allow_output_tools,
        schema
    ));
    hash.update(format!(
        "{:?}|{:?}",
        request.additional_params, request.tools
    ));
    // Don't put book text or image data in cache keys.
    hash.update([u8::from(request.chat_history.iter().any(|m| matches!(m, Message::User { content } if content.iter().any(|c| matches!(c, UserContent::Image(_))))))]);
    format!("{:x}", hash.finalize())
}

fn unsupported_modes(key: &str) -> HashSet<OutputMode> {
    let mut cache = CAPABILITIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.retain(|_, (time, _)| time.elapsed() < CACHE_TTL);
    cache
        .get(key)
        .map(|(_, modes)| modes.clone())
        .unwrap_or_default()
}

fn remember_unsupported(key: &str, mode: OutputMode) {
    let mut cache = CAPABILITIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() >= 256 {
        cache.clear();
    }
    cache
        .entry(key.to_owned())
        .or_insert_with(|| (Instant::now(), HashSet::new()))
        .1
        .insert(mode);
}

fn modes(provider: &AiProvider, schema: &Value, has_tools: bool) -> Vec<OutputMode> {
    use AiProviderKind as P;
    use OutputMode as M;
    if provider.structured_output != M::Auto {
        return vec![provider.structured_output];
    }
    if provider.kind == P::Custom {
        return vec![M::Prompt];
    }
    [M::Native, M::Tool, M::JsonObject, M::Prompt]
        .into_iter()
        .filter(|mode| match mode {
            M::Native => {
                !matches!(provider.kind, P::DeepSeek | P::Moonshot | P::MiniMax)
                    && !has_tools
                    && native_schema_compatible(schema, provider.kind)
            }
            M::Tool => provider.allow_output_tools && !has_tools,
            M::JsonObject => !matches!(provider.kind, P::Anthropic),
            _ => true,
        })
        .collect()
}

// Conservative grammar subset. Never strip constraints to make a schema fit.
// Strict OpenAI dialects require closed objects with all properties required.
fn native_schema_compatible(schema: &Value, kind: AiProviderKind) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    if schema["type"] == "object"
        && !object.contains_key("properties")
        && !matches!(kind, AiProviderKind::Gemini | AiProviderKind::Ollama)
    {
        return false;
    }
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "type"
                | "title"
                | "description"
                | "properties"
                | "required"
                | "additionalProperties"
                | "items"
                | "enum"
                | "anyOf"
                | "$schema"
                | "minLength"
                | "maxLength"
                | "minimum"
                | "maximum"
                | "minItems"
                | "maxItems"
        )
    }) {
        return false;
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        if !matches!(kind, AiProviderKind::Gemini | AiProviderKind::Ollama) {
            let required = object.get("required").and_then(Value::as_array);
            if object.get("additionalProperties") != Some(&Value::Bool(false))
                || properties
                    .keys()
                    .any(|key| !required.is_some_and(|r| r.contains(&Value::String(key.clone()))))
            {
                return false;
            }
        }
        if properties
            .values()
            .any(|v| !native_schema_compatible(v, kind))
        {
            return false;
        }
    }
    if let Some(items) = object.get("items")
        && !native_schema_compatible(items, kind)
    {
        return false;
    }
    if let Some(variants) = object.get("anyOf").and_then(Value::as_array)
        && variants.iter().any(|v| !native_schema_compatible(v, kind))
    {
        return false;
    }
    true
}

pub(super) fn schema_options(schema: Value) -> Value {
    json!({"output_schema":schema})
}

fn reasoning_params(kind: AiProviderKind, model: &str, effort: ReasoningEffort) -> Value {
    let Some(value) = effort.api_value() else {
        return json!({});
    };
    let budget = match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1024,
        ReasoningEffort::Low => 2048,
        ReasoningEffort::Medium => 4096,
        _ => 8192,
    };
    match kind {
        AiProviderKind::Anthropic => {
            if budget == 0 {
                json!({"thinking":{"type":"disabled"}})
            } else {
                json!({"thinking":{"type":"enabled","budget_tokens":budget}})
            }
        }
        AiProviderKind::Gemini => {
            if model.contains("gemini-3") {
                json!({"generationConfig":{"thinkingConfig":{"thinkingLevel":if budget <= 2048 {"low"} else {"high"}}}})
            } else {
                json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":budget}}})
            }
        }
        AiProviderKind::Ollama => json!({"think":budget != 0}),
        AiProviderKind::DeepSeek => {
            if budget == 0 {
                json!({"thinking":{"type":"disabled"}})
            } else {
                json!({"thinking":{"type":"enabled"},"reasoning_effort": match effort {
                    ReasoningEffort::Minimal | ReasoningEffort::Low => "low",
                    ReasoningEffort::Max => "max",
                    _ => "high",
                }})
            }
        }
        AiProviderKind::Moonshot => {
            json!({"thinking":{"type":if budget == 0 {"disabled"} else {"enabled"}}})
        }
        AiProviderKind::MiniMax => json!({}),
        AiProviderKind::Xai => json!({"reasoning":{"effort":value}}),
        _ => json!({"reasoning_effort":value}),
    }
}

pub(crate) fn reasoning_levels(kind: AiProviderKind, model: &str) -> &'static [ReasoningEffort] {
    use ReasoningEffort as R;
    match kind {
        AiProviderKind::MiniMax => &[R::Default],
        AiProviderKind::DeepSeek => &[R::Default, R::None, R::Low, R::High, R::Max],
        AiProviderKind::Ollama | AiProviderKind::Moonshot => &[R::Default, R::None, R::High],
        AiProviderKind::Gemini if model.contains("gemini-3") => &[R::Default, R::Low, R::High],
        AiProviderKind::Gemini if model.contains("pro") => {
            &[R::Default, R::Low, R::Medium, R::High]
        }
        AiProviderKind::OpenAi if model.starts_with("gpt-4") => &[R::Default],
        _ => &ReasoningEffort::ALL,
    }
}

fn build_request(
    provider: &AiProvider,
    model: &str,
    messages: &[Value],
    tools: Option<&Value>,
    max_tokens: Option<u32>,
    effort: ReasoningEffort,
    extra: Option<&Value>,
) -> Result<(CompletionRequest, Option<Value>), String> {
    let mut params = reasoning_params(provider.kind, model, effort);
    if let Some(extra) = extra.and_then(Value::as_object) {
        for (key, value) in extra {
            if key != "response_format"
                && key != "output_schema"
                && key != "best_effort_output_fields"
            {
                params[key] = value.clone();
            }
        }
    }
    let format = extra.and_then(|v| v.get("response_format"));
    let schema = extra.and_then(|v| v.get("output_schema")).cloned().or(
        match format.and_then(|v| v["type"].as_str()) {
            Some("json_schema") => Some(
                format
                    .and_then(|v| v.pointer("/json_schema/schema"))
                    .ok_or("缺少输出 Schema")?
                    .clone(),
            ),
            Some("json_object") => Some(json!({"type":"object"})),
            _ => None,
        },
    );
    let tools = tools
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(|tool| {
                    serde_json::from_value::<ToolDefinition>(tool["function"].clone())
                        .map_err(|e| e.to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let thinking = effort != ReasoningEffort::Default && effort != ReasoningEffort::None;
    let temperature = params
        .as_object_mut()
        .and_then(|p| p.remove("temperature"))
        .and_then(|v| v.as_f64());
    // Do not add our sampling default to models that reason by default and
    // reject it. Explicit caller options still go through SDK validation.
    let sampling = rig_core::providers::registry::ProviderId::catalog("openai")
        .and_then(|id| rig_core::catalog::Catalog::builtin().find(id, model));
    let default_temperature = sampling.is_none_or(|spec| match spec.sampling {
        Some(rig_core::catalog::Sampling::Never) => false,
        Some(rig_core::catalog::Sampling::ReasoningOff) => {
            effort == ReasoningEffort::None || spec.reasoning.default.is_none()
        }
        _ => true,
    });
    let thinking_budget = params
        .pointer("/thinking/budget_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut request = CompletionRequest::new("");
    request.chat_history = history::messages(provider, model, messages)?;
    request.tool_choice = (!tools.is_empty()).then_some(ToolChoice::Auto);
    request.tools = tools;
    request.temperature = if thinking {
        None
    } else {
        temperature.or(default_temperature.then_some(0.2))
    };
    request.max_tokens = Some(
        max_tokens
            .map(u64::from)
            .unwrap_or(16384)
            .saturating_add(thinking_budget),
    );
    request.additional_params = Some(params);
    Ok((request, schema))
}

fn prepare_output(
    mut request: CompletionRequest,
    schema: &Value,
    mode: OutputMode,
    kind: AiProviderKind,
) -> Result<(CompletionRequest, bool), String> {
    let wrapped =
        mode != OutputMode::Prompt && schema.get("type").and_then(Value::as_str) != Some("object");
    let wire_schema = if wrapped {
        json!({"type":"object","properties":{"result":schema},"required":["result"],"additionalProperties":false})
    } else {
        schema.clone()
    };
    match mode {
        OutputMode::Native => {
            request.output_schema = Some(
                wire_schema
                    .try_into()
                    .map_err(|e| format!("Schema 无效：{e}"))?,
            );
        }
        OutputMode::Tool => {
            if !request.tools.is_empty() {
                return Err("结构化结果工具不能覆盖已有业务工具".into());
            }
            request.tools = vec![ToolDefinition {
                name: rig_core::message::ToolName::new(OUTPUT_TOOL).expect("nonempty tool name"),
                description: "Return the final structured result; this does not execute an action."
                    .into(),
                parameters: wire_schema,
            }];
            request.tool_choice = Some(ToolChoice::Specific {
                function_names: vec![
                    rig_core::message::ToolName::new(OUTPUT_TOOL).expect("nonempty tool name"),
                ],
            });
        }
        OutputMode::JsonObject | OutputMode::Prompt => {
            if mode == OutputMode::JsonObject {
                let params = request.additional_params.get_or_insert_with(|| json!({}));
                match kind {
                    AiProviderKind::Gemini => {
                        params["generationConfig"]["responseMimeType"] = json!("application/json");
                    }
                    AiProviderKind::Ollama => {
                        params["format"] = json!("json");
                    }
                    AiProviderKind::Xai => {
                        params["text"] = json!({"format":{"type":"json_object"}});
                    }
                    _ => {
                        params["response_format"] = json!({"type":"json_object"});
                    }
                }
            }
            request.chat_history.push(Message::system(format!(
                "Return one complete JSON value that follows this Schema and its field descriptions. Omit Markdown fences and commentary. JSON Schema:\n{wire_schema}")));
        }
        OutputMode::Auto => return Err("内部错误：未选择结构化输出模式".into()),
    }
    let mut instructions = Vec::new();
    request.chat_history.retain(|message| {
        if let Message::System { content } = message {
            instructions.push(content.clone());
            false
        } else {
            true
        }
    });
    if !instructions.is_empty() {
        request
            .chat_history
            .insert(0, Message::system(instructions.join("\n\n")));
    }
    Ok((request, wrapped))
}

fn can_fallback(error: &CompletionError) -> bool {
    let status = error
        .provider_response_status()
        .map(|status| status.as_u16());
    if !matches!(status, Some(400 | 404 | 422)) {
        return false;
    }
    let detail = error
        .provider_response_body()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let capability = [
        "response_format",
        "json_schema",
        "structured output",
        "tool_choice",
        "tools",
        "function calling",
        "responsemimetype",
        "\"format\"",
    ]
    .iter()
    .any(|word| detail.contains(word));
    let unsupported = [
        "not supported",
        "unsupported",
        "not available",
        "unknown parameter",
        "unrecognized",
        "not allowed",
    ]
    .iter()
    .any(|word| detail.contains(word));
    capability && unsupported
}

fn check_finish(response: &CompletionResponse) -> Result<(), String> {
    if response
        .tool_calls()
        .any(|c| c.function.name != OUTPUT_TOOL && c.function.invalid_arguments.is_some())
    {
        return Err("AI 工具参数不是有效 JSON，未执行工具".into());
    }
    if let rig_core::message::StopReason::Error(reason)
    | rig_core::message::StopReason::Aborted(reason) = response.stop()
    {
        return Err(format!("AI 响应未正常完成，未应用结果：{reason}"));
    }
    match response.finish_reason() {
        Some(FinishReason::Length) => Err("AI 输出被长度上限截断，未应用结果".into()),
        Some(FinishReason::ContentFilter) => Err("AI 拒绝或过滤了本次请求，未应用结果".into()),
        _ => Ok(()),
    }
}

fn response_message(response: &CompletionResponse) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tools = Vec::new();
    for part in &response.choice {
        match part {
            AssistantContent::Text(t) => text.push_str(&t.text),
            AssistantContent::Reasoning(r) => reasoning.push_str(&r.text),
            AssistantContent::ToolCall(t) => tools.push(json!({"id":t.id.wire(),"type":"function","function":{"name":t.function.name,"arguments":t.function.invalid_arguments.clone().unwrap_or_else(|| t.function.arguments_value().to_string())}})),
            _ => {},
        }
    }
    let mut message = json!({"role":"assistant","content":text,"reasoning_content":reasoning});
    if !tools.is_empty() {
        message["tool_calls"] = json!(tools);
    }
    message["_rig_message"] = serde_json::to_value(response.message()).unwrap_or_default();
    message["_rig_fingerprint"] =
        serde_json::to_value(rig_core::message::Fingerprint::of(&response.message()))
            .unwrap_or_default();
    if let Some(scope) = response.raw.get("_torto_llm_scope") {
        message["_llm_scope"] = scope.clone();
    }
    let mut sources = Vec::new();
    super::web_search::collect_sources(&response.raw, &mut sources);
    super::web_search::collect_sources(&message["_rig_message"], &mut sources);
    message["_web_sources"] = json!(sources);
    message
}

fn structured_value(
    response: &CompletionResponse,
    mode: OutputMode,
    wrapped: bool,
) -> Result<Value, String> {
    if mode == OutputMode::Tool {
        let calls: Vec<_> = response
            .choice
            .iter()
            .filter_map(|part| {
                if let AssistantContent::ToolCall(call) = part {
                    Some(call)
                } else {
                    None
                }
            })
            .collect();
        if calls.len() != 1 || calls[0].function.name != OUTPUT_TOOL {
            return Err("AI 未返回唯一的结构化结果工具调用".into());
        }
        let mut result = if let Some(raw) = &calls[0].function.invalid_arguments {
            llm_json::parse(raw)?
        } else {
            calls[0].function.arguments_value()
        };
        if let Some(text) = result.as_str() {
            result = llm_json::parse(text)?;
        }
        return if wrapped {
            result
                .get("result")
                .cloned()
                .ok_or_else(|| "结构化结果缺少 result".into())
        } else {
            Ok(result)
        };
    }
    let text: String = response
        .choice
        .iter()
        .filter_map(|part| {
            if let AssistantContent::Text(t) = part {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .collect();
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => Ok(value),
        Err(_) => llm_json::parse(&text).map_err(|e| format!("结构化输出不是有效 JSON：{e}")),
    }?;
    if wrapped {
        value
            .get("result")
            .cloned()
            .ok_or_else(|| "结构化结果缺少 result".into())
    } else {
        Ok(value)
    }
}

// Wire schemas stay strict for providers. Callers may validate optional enrichment
// themselves, without throwing away valid primary output or making a paid retry.
fn best_effort_validation_schema(schema: &Value, extra: Option<&Value>) -> Value {
    let mut validation = schema.clone();
    if let Some(fields) = extra
        .and_then(|v| v.get("best_effort_output_fields"))
        .and_then(Value::as_array)
    {
        for field in fields.iter().filter_map(Value::as_str) {
            if validation
                .get("properties")
                .and_then(|v| v.get(field))
                .is_none()
            {
                continue;
            }
            validation["properties"][field] = json!({});
            if let Some(required) = validation.get_mut("required").and_then(Value::as_array_mut) {
                required.retain(|v| v.as_str() != Some(field));
            }
        }
    }
    validation
}

pub(super) async fn complete(
    provider: &AiProvider,
    model: &str,
    messages: &[Value],
    tools: Option<&Value>,
    max_tokens: Option<u32>,
    effort: ReasoningEffort,
    extra: Option<&Value>,
) -> Result<Value, String> {
    let (request, schema) =
        build_request(provider, model, messages, tools, max_tokens, effort, extra)?;
    if schema.is_some()
        && provider.structured_output == OutputMode::Native
        && matches!(
            provider.kind,
            AiProviderKind::DeepSeek | AiProviderKind::Moonshot | AiProviderKind::MiniMax
        )
    {
        return Err("当前适配器不支持 Native JSON Schema，请选择 Auto 或其他输出模式".into());
    }
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let Some(schema) = schema else {
            let response = dispatch(provider, model, request, &mut |_| {}, false)
                .await
                .map_err(|e| e.to_string())?;
            check_finish(&response)?;
            return Ok(response_message(&response));
        };
        let validation_schema = best_effort_validation_schema(&schema, extra);
        let validator = jsonschema::validator_for(&validation_schema)
            .map_err(|e| format!("输出 Schema 无效：{e}"))?;
        let key = cache_key(provider, model, &schema, &request);
        let disabled = unsupported_modes(&key);
        let strategies: Vec<_> = modes(provider, &schema, !request.tools.is_empty())
            .into_iter()
            .filter(|m| !disabled.contains(m) || provider.structured_output != OutputMode::Auto)
            // Forced tools and extended thinking cannot be combined by Anthropic.
            .filter(|m| {
                !(*m == OutputMode::Tool
                    && provider.kind == AiProviderKind::Anthropic
                    && request
                        .additional_params
                        .as_ref()
                        .and_then(|p| p.pointer("/thinking/type"))
                        .is_some_and(|v| v == "enabled"))
            })
            .collect();
        let mut last_error = String::new();
        if strategies.is_empty() {
            return Err("当前输出模式与请求配置不兼容，请选择 Auto 或关闭扩展思考".into());
        }
        for (attempt, mode) in strategies.into_iter().enumerate() {
            let started = Instant::now();
            crate::diagnostics::log(
                "llm.request",
                &[
                    crate::diagnostics::Field::Detail("provider", &provider.id),
                    crate::diagnostics::Field::Detail("model", model),
                    crate::diagnostics::Field::Text("output", mode.label()),
                    crate::diagnostics::Field::Usize("attempt", attempt + 1),
                ],
            );
            let (wire_request, wrapped) =
                prepare_output(request.clone(), &schema, mode, provider.kind)?;
            match dispatch(provider, model, wire_request, &mut |_| {}, false).await {
                Ok(response) => {
                    check_finish(&response)?;
                    let value = structured_value(&response, mode, wrapped)?;
                    validator.validate(&value).map_err(|e| {
                        format!("AI 输出 Schema 校验失败：{} · {}", e.instance_path(), e)
                    })?;
                    let mut message = response_message(&response);
                    message["content"] = Value::String(value.to_string());
                    message["_structured_value"] = value;
                    message["_llm_attempts"] = json!(attempt + 1);
                    crate::diagnostics::log(
                        "llm.complete",
                        &[
                            crate::diagnostics::Field::Text("output", mode.label()),
                            crate::diagnostics::Field::U64(
                                "elapsed_ms",
                                started.elapsed().as_millis() as u64,
                            ),
                        ],
                    );
                    return Ok(message);
                }
                Err(error) => {
                    if provider.structured_output != OutputMode::Auto || !can_fallback(&error) {
                        return Err(error.to_string());
                    }
                    last_error = error.to_string();
                    remember_unsupported(&key, mode);
                    crate::diagnostics::log(
                        "llm.output_fallback",
                        &[
                            crate::diagnostics::Field::Text("from", mode.label()),
                            crate::diagnostics::Field::Detail("reason", &last_error),
                        ],
                    );
                }
            }
        }
        Err(format!("没有可用的结构化输出方式：{last_error}"))
    })
    .await
    .map_err(|_| "AI 请求超时".to_owned())?
}

pub(super) async fn stream(
    provider: &AiProvider,
    model: &str,
    messages: &[Value],
    tools: Option<&Value>,
    effort: ReasoningEffort,
    callback: &mut impl FnMut(ChatStreamEvent),
) -> Result<Value, String> {
    stream_with_search(provider, model, messages, tools, effort, None, callback).await
}

pub(super) async fn stream_with_search(
    provider: &AiProvider,
    model: &str,
    messages: &[Value],
    tools: Option<&Value>,
    effort: ReasoningEffort,
    search: Option<&Value>,
    callback: &mut impl FnMut(ChatStreamEvent),
) -> Result<Value, String> {
    // Model-name inference selects search declarations, never the gateway protocol.
    let (request, _) = build_request(provider, model, messages, tools, None, effort, search)?;
    let response = tokio::time::timeout(
        REQUEST_TIMEOUT,
        dispatch(provider, model, request, callback, true),
    )
    .await
    .map_err(|_| "AI 请求超时".to_owned())?
    .map_err(|e| e.to_string())?;
    check_finish(&response)?;
    Ok(response_message(&response))
}

async fn execute<W>(
    model: Model<W>,
    request: CompletionRequest,
    callback: &mut impl FnMut(ChatStreamEvent),
    streaming: bool,
) -> Result<CompletionResponse, CompletionError>
where
    W: Wire<Op = Completion, Payload = Encoded, Frame = WireFrame>,
{
    if !streaming {
        return model.call(request).await;
    }
    let mut stream = model.stream(request)?;
    let mut text = String::new();
    let mut text_seen = HashSet::new();
    let mut reasoning_seen = HashSet::new();
    let mut sources = Vec::new();
    while let Some(part) = stream.next().await {
        match part? {
            Item::Event(StreamEvent::Text { part, text: delta }) => {
                text_seen.insert(part);
                text.push_str(&delta);
                callback(ChatStreamEvent::Content(text.clone()));
            }
            Item::Event(StreamEvent::Reasoning { part, text }) => {
                reasoning_seen.insert(part);
                callback(ChatStreamEvent::Reasoning(text));
            }
            Item::Event(StreamEvent::End {
                part,
                content: AssistantContent::Text(block),
            }) if !text_seen.contains(&part) => {
                text.push_str(&block.text);
                callback(ChatStreamEvent::Content(text.clone()));
            }
            Item::Event(StreamEvent::End {
                part,
                content: AssistantContent::Reasoning(block),
            }) if !reasoning_seen.contains(&part) => {
                callback(ChatStreamEvent::Reasoning(block.text));
            }
            Item::Event(StreamEvent::End {
                content: AssistantContent::Opaque(block),
                ..
            }) => {
                super::web_search::native_event(&block.item, callback);
                super::web_search::collect_sources(&block.item, &mut sources);
            }
            Item::Unknown(payload) => {
                super::web_search::native_event(payload.value(), callback);
                super::web_search::collect_sources(payload.value(), &mut sources);
            }
            _ => {}
        }
    }
    let mut response = stream.finish().await?;
    if let Some(raw) = response.raw.as_object_mut() {
        raw.insert("_torto_web_sources".into(), json!(sources));
    }
    Ok(response)
}

async fn dispatch(
    provider: &AiProvider,
    model: &str,
    mut request: CompletionRequest,
    callback: &mut impl FnMut(ChatStreamEvent),
    streaming: bool,
) -> Result<CompletionResponse, CompletionError> {
    consume_attempt()?;
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    let http = HTTP
        .get_or_init(|| {
            crate::http::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("HTTP client initialization")
        })
        .clone();
    let source_capture = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = request
        .additional_params
        .as_ref()
        .is_some_and(|p| p.get("tools").is_some());
    let http = transport::CompatHttp {
        inner: rig_reqwest::ReqwestClient::from(http),
        sources: capture.then(|| source_capture.clone()),
    };
    let responses = request
        .additional_params
        .as_mut()
        .and_then(Value::as_object_mut)
        .and_then(|p| p.remove("_torto_responses"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let base = provider
        .base_url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches("/chat/completions");
    // xAI's SDK dialect includes /v1 in its paths; settings include it in the base.
    let base = if provider.kind == AiProviderKind::Xai {
        base.trim_end_matches("/v1")
    } else {
        base
    };
    let key = provider.api_key.trim().to_owned();
    let strict = request.tools.len() == 1
        && request.tools[0].name == OUTPUT_TOOL
        && native_schema_compatible(&request.tools[0].parameters, provider.kind);
    let mut response = match provider.kind {
        AiProviderKind::Anthropic => {
            let client = providers::anthropic::AnthropicConfig::new(key)
                .with_base_url(base)
                .connect(http);
            let mut engine = client.completion(model);
            if strict {
                engine.wire = engine.wire.with_strict_tools();
            }
            execute(engine, request, callback, streaming).await
        }
        AiProviderKind::Gemini => {
            let client = providers::gemini::GeminiConfig::new(key)
                .with_base_url(base)
                .connect(http);
            execute(client.completion(model), request, callback, streaming).await
        }
        AiProviderKind::Ollama => {
            let client = providers::ollama::OllamaConfig::new()
                .with_base_url(base)
                .with_api_key(key)
                .connect(http);
            execute(
                client.native_completion(model),
                request,
                callback,
                streaming,
            )
            .await
        }
        _ => {
            use providers::openai::wire::{OpenAIConfig, Route};
            let dialect = match provider.kind {
                AiProviderKind::DeepSeek => &providers::openai::wire::DEEPSEEK,
                AiProviderKind::OpenRouter => &providers::openai::wire::OPENROUTER,
                AiProviderKind::Xai => &providers::xai::DIALECT,
                AiProviderKind::Groq => &providers::openai::wire::GROQ,
                AiProviderKind::Mistral => &providers::openai::wire::MISTRAL,
                AiProviderKind::Moonshot => &providers::openai::wire::MOONSHOT,
                AiProviderKind::MiniMax => &providers::openai::wire::MINIMAX,
                AiProviderKind::Zai => &providers::openai::wire::ZAI,
                _ => &providers::openai::wire::OPENAI,
            };
            let config = OpenAIConfig::with_key(dialect, key).with_base_url(base);
            // A gateway's model name never changes its configured API protocol.
            let config = if matches!(
                provider.kind,
                AiProviderKind::OpenAi | AiProviderKind::Custom
            ) {
                config.with_route(if responses && provider.kind == AiProviderKind::OpenAi {
                    Route::Responses
                } else {
                    Route::Chat
                })
            } else {
                config
            };
            let client = config.connect(http);
            let mut engine = client.completion(model);
            if strict && provider.kind == AiProviderKind::OpenAi {
                engine.wire = engine.wire.with_strict_tools();
            }
            execute(engine, request, callback, streaming).await
        }
    }?;
    if !response.raw.is_object() {
        response.raw = json!({});
    }
    response.raw["_torto_llm_scope"] = json!(history::scope(provider, model));
    if capture {
        response.raw["_torto_captured_sources"] = json!(
            *source_capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );
    }
    Ok(response)
}
