use super::{
    images,
    model::{Context, Draft, Kind, Payload},
    transport::{self, Failure, Outcome},
};
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn remaining_retry(draft: &Draft) -> Option<Duration> {
    draft
        .retry_until
        .and_then(|at| at.checked_sub(now_seconds()))
        .filter(|s| *s > 0)
        .map(|s| Duration::from_secs(s.min(604800)))
}
pub fn new_draft(context: Context) -> Result<Draft, String> {
    let endpoint = std::env::var("TORTO_FEEDBACK_URL")
        .ok()
        .or_else(|| option_env!("TORTO_FEEDBACK_URL").map(str::to_owned))
        .unwrap_or_else(|| transport::DEFAULT_ENDPOINT.into());
    transport::endpoint(&endpoint)?;
    Ok(Draft {
        version: 1,
        endpoint,
        retry_until: None,
        payload: Payload {
            request_id: uuid::Uuid::new_v4().to_string(),
            kind: Kind::Bug,
            description: String::new(),
            screenshots: vec![],
            context,
        },
    })
}
pub fn validate(draft: &Draft, submitted: bool) -> Result<(), String> {
    transport::endpoint(&draft.endpoint)?;
    if draft.version != 1
        || uuid::Uuid::parse_str(&draft.payload.request_id).is_err()
        || draft.payload.description.encode_utf16().count() > 10000
        || submitted && !super::model::valid_description(&draft.payload.description)
        || draft.payload.screenshots.len() > images::MAX_IMAGES
    {
        return Err("storage".into());
    }
    for image in &draft.payload.screenshots {
        images::decode(image)?;
    }
    Ok(())
}
pub fn load(path: &Path, submitted: bool) -> Result<Option<Draft>, String> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(m) if m.len() <= 9 * 1024 * 1024 => {}
        _ => return Err("storage".into()),
    }
    let draft = serde_json::from_slice(&std::fs::read(path).map_err(|_| "storage")?)
        .map_err(|_| "storage")?;
    validate(&draft, submitted)?;
    Ok(Some(draft))
}
pub fn save(path: &Path, draft: &Draft) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("draft directory"))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    crate::persistence::write_json_atomic(path, draft)
}
/// Persist the exact request before HTTP. Ambiguous results retain its ID and
/// frozen payload so retrying never creates a second issue.
pub async fn send_saved(
    path: std::path::PathBuf,
    mut draft: Draft,
) -> Result<(Draft, Outcome), String> {
    let saved = draft.clone();
    let saved_path = path.clone();
    tokio::task::spawn_blocking(move || {
        validate(&saved, true)?;
        save(&saved_path, &saved).map_err(|_| "storage".to_owned())
    })
    .await
    .map_err(|_| "storage")??;
    let result = transport::submit(&draft).await;
    let retry = match &result {
        Outcome::Pending(wait) => Some(*wait),
        Outcome::Failed(_, wait) => *wait,
        _ => None,
    };
    draft.retry_until = retry.map(|wait| now_seconds() + wait.as_secs());
    let saved = draft.clone();
    let remove = matches!(
        result,
        Outcome::Submitted(_) | Outcome::Failed(Failure::Invalid, _)
    );
    tokio::task::spawn_blocking(move || {
        if remove {
            match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        } else {
            save(&path, &saved)
        }
    })
    .await
    .map_err(|_| "storage")?
    .map_err(|_| "storage")?;
    Ok((draft, result))
}
