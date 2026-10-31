//! Adapt in-memory JSON messages; Rig owns native items and replay.
use super::*;
use rig_core::message::{
    AssistantMessage, DocumentSourceKind, Image, ImageDetail, ImageMediaType, StopReason, ToolCall,
    ToolFunction, ToolName,
};

pub(super) fn scope(provider: &AiProvider, model: &str) -> String {
    let mut hash = Sha256::new();
    for value in [
        format!("{:?}", provider.kind),
        provider.id.clone(),
        provider.base_url.trim().trim_end_matches('/').to_owned(),
        provider.api_key.trim().to_owned(),
        model.to_owned(),
    ] {
        hash.update(value.len().to_le_bytes());
        hash.update(value);
    }
    format!("{:x}", hash.finalize())
}

fn text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant(value: &Value) -> Result<Message, String> {
    let mut content = Vec::new();
    let reasoning = ["reasoning_content", "reasoning", "reasoning_text"]
        .into_iter()
        .find_map(|key| {
            value[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| (key, s))
        });
    if reasoning.is_some()
        || value["reasoning_details"]
            .as_array()
            .is_some_and(|d| !d.is_empty())
    {
        let block = AssistantContent::reasoning(reasoning.map_or("", |(_, t)| t));
        content.push(block);
    }
    let answer = text(&value["content"]);
    if !answer.is_empty() {
        content.push(AssistantContent::text(answer));
    }
    for raw in value["tool_calls"].as_array().into_iter().flatten() {
        let function = &raw["function"];
        let name = ToolName::new(function["name"].as_str().unwrap_or_default())
            .map_err(|e| e.to_string())?;
        let function = ToolFunction::new(name, function["arguments"].clone());
        let call = ToolCall::from_wire(raw["id"].as_str().unwrap_or_default(), function);
        let block = AssistantContent::ToolCall(call);
        content.push(block);
    }
    let has_tools = content
        .iter()
        .any(|p| matches!(p, AssistantContent::ToolCall(_)));
    let turn = AssistantMessage::new(content).with_stop(Some(if has_tools {
        StopReason::ToolUse
    } else {
        StopReason::Stop
    }));
    Ok(Message::Assistant(turn))
}

fn user(value: &Value) -> Result<Message, String> {
    let Some(parts) = value.as_array() else {
        return Ok(Message::user(text(value)));
    };
    let mut content = Vec::new();
    for part in parts {
        match part["type"].as_str() {
            Some("text") => {
                content.push(UserContent::text(part["text"].as_str().unwrap_or_default()))
            }
            Some("image_url") => {
                let url = part["image_url"]["url"].as_str().ok_or("AI 图片缺少 URL")?;
                let (data, media_type) = if let Some(raw) = url.strip_prefix("data:") {
                    let (mime, bytes) = raw.split_once(";base64,").ok_or("AI 图片数据格式无效")?;
                    let kind = match mime {
                        "image/jpeg" => ImageMediaType::JPEG,
                        "image/png" => ImageMediaType::PNG,
                        "image/webp" => ImageMediaType::WEBP,
                        "image/gif" => ImageMediaType::GIF,
                        _ => return Err("AI 图片类型不支持".into()),
                    };
                    (DocumentSourceKind::base64(bytes), Some(kind))
                } else {
                    (DocumentSourceKind::url(url), None)
                };
                let detail = part["image_url"]
                    .get("detail")
                    .cloned()
                    .map(serde_json::from_value::<ImageDetail>)
                    .transpose()
                    .map_err(|e| e.to_string())?;
                content.push(UserContent::Image(Image {
                    data,
                    media_type,
                    detail,
                    ..Default::default()
                }));
            }
            _ => return Err("AI 请求包含不支持的内容类型".into()),
        }
    }
    Ok(Message::User { content })
}

pub(super) fn messages(
    provider: &AiProvider,
    model: &str,
    values: &[Value],
) -> Result<Vec<Message>, String> {
    let current_scope = scope(provider, model);
    let mut output = Vec::new();
    for value in values {
        let mut message = if let Some(core) = value.get("_rig_message") {
            serde_json::from_value::<Message>(core.clone())
                .map_err(|e| format!("AI 历史消息无效：{e}"))?
        } else {
            match value["role"].as_str() {
                Some("system" | "developer") => Message::system(text(&value["content"])),
                Some("user") => user(&value["content"])?,
                Some("assistant") => assistant(value)?,
                Some("tool") => {
                    let id = value["tool_call_id"].as_str().unwrap_or_default();
                    let call = output
                        .iter()
                        .rev()
                        .find_map(|m| match m {
                            Message::Assistant(turn) => {
                                turn.tool_calls().find(|c| c.id.wire() == id)
                            }
                            _ => None,
                        })
                        .ok_or("AI 工具结果没有对应的调用")?;
                    Message::tool_results(vec![
                        call.result(vec![ToolResultContent::text(text(&value["content"]))]),
                    ])
                }
                _ => return Err("AI 请求消息角色无效".into()),
            }
        };
        if value.get("_rig_message").is_some()
            && (value["_llm_scope"] != current_scope
                || value["_rig_fingerprint"]
                    != serde_json::to_value(rig_core::message::Fingerprint::of(&message))
                        .unwrap_or_default())
        {
            if let Message::Assistant(turn) = &mut message {
                turn.content.retain(|p| {
                    !matches!(
                        p,
                        AssistantContent::Reasoning(_) | AssistantContent::Opaque(_)
                    )
                });
                turn.content = turn
                    .content
                    .iter()
                    .map(AssistantContent::canonical)
                    .collect();
                turn.origin = None;
            }
        }
        output.push(message);
    }
    Ok(output)
}
