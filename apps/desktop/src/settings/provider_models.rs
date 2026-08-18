use std::time::Duration;

use serde::Deserialize;

use crate::async_task::TaskResult;

#[derive(Clone)]
pub(super) struct ProviderModelsRequest {
    pub(super) provider_id: String,
    pub(super) base_url: String,
    pub(super) api_key: String,
}

pub(crate) type ProviderModelsMessage = TaskResult<Vec<String>>;

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

pub(super) async fn fetch_provider_models(
    request: &ProviderModelsRequest,
) -> Result<Vec<String>, String> {
    let url = models_url(&request.base_url)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("创建模型请求失败：{error}"))?;
    let mut builder = client.get(&url);
    if !request.api_key.trim().is_empty() {
        builder = builder.bearer_auth(request.api_key.trim());
    }
    let response = builder
        .send()
        .await
        .map_err(|error| format!("获取模型失败：{error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("读取模型响应失败：{error}"))?;
    if !status.is_success() {
        let detail = body.trim();
        return Err(if detail.is_empty() {
            format!("获取模型失败：HTTP {status}")
        } else {
            format!(
                "获取模型失败：HTTP {status} · {}",
                clipped_error_detail(detail)
            )
        });
    }
    parse_models_response(&body)
}

fn clipped_error_detail(detail: &str) -> String {
    const MAX_CHARS: usize = 240;
    if detail.chars().count() <= MAX_CHARS {
        return detail.to_owned();
    }
    let end = detail
        .char_indices()
        .nth(MAX_CHARS)
        .map_or(detail.len(), |(index, _)| index);
    format!("{}…", &detail[..end])
}

fn models_url(base_url: &str) -> Result<String, String> {
    let mut base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("API 地址不能为空".into());
    }
    if !base.starts_with("https://") && !base.starts_with("http://") {
        return Err("API 地址必须使用 http:// 或 https://".into());
    }
    if let Some(prefix) = base.strip_suffix("/chat/completions") {
        base = prefix.trim_end_matches('/');
    }
    if base.ends_with("/models") {
        Ok(base.to_owned())
    } else {
        Ok(format!("{base}/models"))
    }
}

fn parse_models_response(body: &str) -> Result<Vec<String>, String> {
    let response: ModelsResponse =
        serde_json::from_str(body).map_err(|error| format!("模型接口响应格式无效：{error}"))?;
    let mut models = response
        .data
        .into_iter()
        .map(|model| model.id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.cmp(right))
    });
    models.dedup();
    Ok(models)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn models_endpoint_is_normalized_from_common_base_urls() {
        assert_eq!(
            models_url("https://example.com/v1").unwrap(),
            "https://example.com/v1/models"
        );
        assert_eq!(
            models_url("https://example.com/v1/chat/completions/").unwrap(),
            "https://example.com/v1/models"
        );
        assert_eq!(
            models_url("https://example.com/v1/models/").unwrap(),
            "https://example.com/v1/models"
        );
    }

    #[test]
    fn model_ids_are_trimmed_deduplicated_and_sorted() {
        let models = parse_models_response(
            r#"{"data":[{"id":" zeta "},{"id":"Alpha"},{"id":""},{"id":"Alpha"}]}"#,
        )
        .unwrap();
        assert_eq!(models, ["Alpha", "zeta"]);
    }

    #[test]
    fn fetch_uses_models_path_and_bearer_authentication() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /v1/models HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer secret")
            );
            let body = r#"{"data":[{"id":"model-b"},{"id":"model-a"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let request = ProviderModelsRequest {
            provider_id: "provider".into(),
            base_url: format!("http://{address}/v1/chat/completions"),
            api_key: "secret".into(),
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let models = runtime.block_on(fetch_provider_models(&request)).unwrap();
        server.join().unwrap();
        assert_eq!(models, ["model-a", "model-b"]);
    }
}
