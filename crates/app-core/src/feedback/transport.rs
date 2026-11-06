use super::model::Draft;
use reqwest::Url;
use std::time::Duration;

pub const DEFAULT_ENDPOINT: &str = "https://read.rethinkos.com/api/feedback";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Network,
    Service,
    Invalid,
    Conflict,
    RateLimited,
    Protocol,
    Storage,
    Configuration,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Submitted(Issue),
    Pending(Duration),
    Failed(Failure, Option<Duration>),
}

pub fn endpoint(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "configuration")?;
    let private = url
        .host_str()
        .and_then(|s| s.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => {
                ip.is_loopback()
                    || ip.is_private()
                    || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
            }
            std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
        });
    if url.host_str().is_none()
        || !(url.scheme() == "https" || url.scheme() == "http" && private)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/api/feedback"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("configuration".into());
    }
    Ok(url)
}

pub async fn submit(draft: &Draft) -> Outcome {
    let Ok(url) = endpoint(&draft.endpoint) else {
        return Outcome::Failed(Failure::Configuration, None);
    };
    let Ok(bytes) = serde_json::to_vec(&draft.payload) else {
        return Outcome::Failed(Failure::Invalid, None);
    };
    if bytes.len() > 9 * 1024 * 1024 {
        return Outcome::Failed(Failure::Invalid, None);
    }
    let client = match crate::http::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(_) => return Outcome::Failed(Failure::Network, None),
    };
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("User-Agent", concat!("Torto/", env!("CARGO_PKG_VERSION")))
        .body(bytes)
        .send()
        .await;
    let Ok(mut response) = response else {
        return Outcome::Failed(Failure::Network, None);
    };
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(|s| Duration::from_secs(s.clamp(1, 604_800)));
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() <= 16 * 1024 => {
                body.extend_from_slice(&chunk)
            }
            Ok(None) => break,
            _ => return Outcome::Failed(Failure::Protocol, retry),
        }
    }
    parse_response(status, &body, &draft.payload.request_id, retry)
}

pub fn parse_response(status: u16, body: &[u8], id: &str, retry: Option<Duration>) -> Outcome {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    if matches!(status, 200..=202) {
        if json["requestId"].as_str() != Some(id) {
            return Outcome::Failed(Failure::Protocol, retry);
        }
        if status == 202 && json["status"] == "pending" {
            return Outcome::Pending(retry.unwrap_or(Duration::from_secs(30)));
        }
        if status != 202
            && json["status"] == "submitted"
            && let Some(number) = json["issue"]["number"].as_u64().filter(|n| *n > 0)
        {
            let url = format!("https://github.com/TortoTech/torto/issues/{number}");
            if json["issue"]["url"].as_str() == Some(url.as_str()) {
                return Outcome::Submitted(Issue { number, url });
            }
        }
        return Outcome::Failed(Failure::Protocol, retry);
    }
    let kind = match status {
        400 | 413 | 415 => Failure::Invalid,
        409 => Failure::Conflict,
        429 => Failure::RateLimited,
        _ => Failure::Service,
    };
    Outcome::Failed(kind, retry)
}
