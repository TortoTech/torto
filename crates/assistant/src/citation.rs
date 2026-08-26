use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::Value;

/// Stable application citation URI understood by Torto navigation adapters.
pub const CHAT_CITATION_PREFIX: &str = "link://j/";

/// Shared model contract for source-backed answers.
pub const CHAT_CITATION_INSTRUCTION: &str = "# 引用\n工具和用户引用会提供 OpenAI 风格的 citation 标记。引用书中内容时，逐字复制对应的完整标记。正确示例：`【18/n104†source】`。不要编造 citation、unit 或 id。总结中的每个主要主题、概念或结论都要就近引用。多个引用连续出现时，让完整标记直接相邻，例如 `【18/n104†source】【19/n205†source】`。输出前检查：涉及书中内容时，每个引用都必须是资料中已经提供的完整 citation 标记。";

const CITATION_COMPONENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// Builds a durable citation URI for a section or one normalized source node.
#[must_use]
pub fn chat_citation_link(section_index: usize, node: Option<&str>) -> String {
    node.map_or_else(
        || format!("{CHAT_CITATION_PREFIX}{section_index}"),
        |node| {
            format!(
                "{CHAT_CITATION_PREFIX}{section_index}/{}",
                utf8_percent_encode(node, CITATION_COMPONENT_ENCODE_SET)
            )
        },
    )
}

/// Builds the marker copied verbatim through an OpenAI-compatible response.
#[must_use]
pub fn chat_citation_marker(section_index: usize, node: Option<&str>) -> String {
    let link = chat_citation_link(section_index, node);
    let locator = link.strip_prefix(CHAT_CITATION_PREFIX).unwrap_or(&link);
    format!("【{locator}†source】")
}

/// Converts one Torto citation URI to the model-visible source marker.
#[must_use]
pub fn chat_citation_marker_from_link(link: &str) -> Option<String> {
    let locator = link.strip_prefix(CHAT_CITATION_PREFIX)?;
    let (section, node) = locator
        .split_once('/')
        .map_or((locator, None), |(section, node)| (section, Some(node)));
    if section.is_empty()
        || !section.bytes().all(|byte| byte.is_ascii_digit())
        || node.is_some_and(str::is_empty)
    {
        return None;
    }
    Some(format!("【{locator}†source】"))
}

/// Rewrites application citation URI fields in a JSON tool result to the
/// provider-facing marker fields consumed by the shared prompt contract.
#[must_use]
pub fn citations_for_model(mut value: Value) -> Value {
    replace_citation_links(&mut value);
    value
}

fn replace_citation_links(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(marker) = object
                .get("href")
                .and_then(Value::as_str)
                .and_then(chat_citation_marker_from_link)
            {
                object.remove("href");
                object.insert("citation".into(), Value::String(marker));
            }
            if let Some(markers) = object.get("hrefs").and_then(Value::as_array).map(|links| {
                links
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(chat_citation_marker_from_link)
                    .map(Value::String)
                    .collect::<Vec<_>>()
            }) {
                object.remove("hrefs");
                object.insert("citations".into(), Value::Array(markers));
            }
            for child in object.values_mut() {
                replace_citation_links(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                replace_citation_links(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn citation_protocol_percent_encodes_source_nodes() {
        let link = chat_citation_link(3, Some("段落 / one"));
        assert_eq!(link, "link://j/3/%E6%AE%B5%E8%90%BD%20%2F%20one");
        assert_eq!(
            chat_citation_marker_from_link(&link).as_deref(),
            Some("【3/%E6%AE%B5%E8%90%BD%20%2F%20one†source】")
        );
    }

    #[test]
    fn tool_results_use_marker_fields_without_losing_nested_citations() {
        let result = citations_for_model(json!({
            "href": "link://j/1/n1",
            "nested": [{ "hrefs": ["link://j/2/n2", "https://invalid.example"] }]
        }));
        assert_eq!(result["citation"], "【1/n1†source】");
        assert_eq!(result["nested"][0]["citations"], json!(["【2/n2†source】"]));
        assert!(result.get("href").is_none());
    }

    #[test]
    fn rejects_non_torto_or_incomplete_citation_links() {
        assert!(chat_citation_marker_from_link("https://example.com").is_none());
        assert!(chat_citation_marker_from_link("link://j/").is_none());
        assert!(chat_citation_marker_from_link("link://j/1/").is_none());
    }
}
