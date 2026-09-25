use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::plugins::AiProvider;

static LOG_LOCK: Mutex<()> = Mutex::new(());
tokio::task_local! { static JOB_ID: String; }

pub(crate) async fn with_job<T>(id: String, future: impl std::future::Future<Output = T>) -> T {
    JOB_ID.scope(id, future).await
}

/// Available in release builds too. Never record request bodies or model text.
pub(crate) fn event(provider: &AiProvider, model: &str, event: &str, mut details: Value) {
    if let Ok(id) = JOB_ID.try_with(Clone::clone) {
        details["job_id"] = json!(id);
    }
    write_event(
        provider,
        model,
        event,
        details,
        "semantic-layout.log",
        "semantic-layout.previous.log",
    );
}

pub(crate) fn translation_event(provider: &AiProvider, model: &str, event: &str, details: Value) {
    write_event(
        provider,
        model,
        event,
        details,
        "translation.log",
        "translation.previous.log",
    );
}

fn write_event(
    provider: &AiProvider,
    model: &str,
    event: &str,
    details: Value,
    filename: &str,
    previous: &str,
) {
    let Some(project) = crate::smoke::project_dirs() else {
        return;
    };
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    let dir = project.data_local_dir().join("logs");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(filename);
    if fs::metadata(&path).is_ok_and(|m| m.len() > 1_048_576) {
        let _ = fs::copy(&path, dir.join(previous));
        if fs::write(&path, []).is_err() {
            return;
        }
    }
    let mut record = json!({"time":chrono::Utc::now().to_rfc3339(), "event":event,
        "provider":provider.id, "model":model, "details":details});
    redact_value(&mut record, provider);
    let line = record.to_string();
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn redact_value(value: &mut Value, provider: &AiProvider) {
    match value {
        Value::String(text) => *text = redact(text, provider).chars().take(2000).collect(),
        Value::Array(values) => {
            for value in values {
                redact_value(value, provider);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_value(value, provider);
            }
        }
        _ => {}
    }
}

fn redact(line: &str, provider: &AiProvider) -> String {
    let mut line = line.to_owned();
    if !provider.api_key.is_empty() {
        line = line.replace(&provider.api_key, "[redacted]");
    }
    if !provider.base_url.is_empty() {
        line = line.replace(&provider.base_url, "[endpoint]");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_secrets_and_endpoint_are_redacted_from_error_details() {
        let provider = AiProvider {
            api_key: "private-key".into(),
            base_url: "https://private.invalid/api".into(),
            ..AiProvider::default()
        };
        assert_eq!(
            redact(
                "request private-key at https://private.invalid/api failed",
                &provider
            ),
            "request [redacted] at [endpoint] failed"
        );
    }
}
