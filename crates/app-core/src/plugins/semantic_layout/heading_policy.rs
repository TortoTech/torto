//! Retired AI heading annotations remain readable, but never affect publication structure.
use super::*;

// Migrate only this known contract transition. All other identity fields (book,
// provider, model, language, reasoning, image policy) must still match exactly.
const LEGACY_CONTRACT: &str = "ec4c993873492cf4eb9f52578828cd50232107170cda39033ab4014b0b57a1a5";
const MIGRATED_CONTRACT: &str = "065bfdc9b41e8632fa89c76938098aa2e1547387426aa0da1e5b12503009aae0";

pub(super) fn annotation_supported(annotation: &Annotation) -> bool {
    !matches!(annotation, Annotation::SectionHeading { .. })
}

pub(super) fn discard(result: &mut Recognition) {
    result.annotations.retain(annotation_supported);
}

fn legacy_identity(identity: &Value) -> Option<Value> {
    let values = identity.as_array()?;
    if values.first()? != &json!(CACHE_FORMAT_VERSION)
        || values.get(1)?.as_str()? != MIGRATED_CONTRACT
    {
        return None;
    }
    let mut legacy = identity.clone();
    legacy[1] = json!(LEGACY_CONTRACT);
    Some(legacy)
}

pub(super) fn presentation_cache_paths(identity: &Value, hash: &str, scopes: bool) -> Vec<PathBuf> {
    std::iter::once(identity.clone())
        .chain(legacy_identity(identity))
        .filter_map(|identity| {
            if scopes {
                scope_presentation_path(&identity, hash)
            } else {
                recognition_path(&identity, hash)
            }
        })
        .collect()
}

pub(super) fn read_presentation_cache(
    identity: &Value,
    hash: &str,
    scopes: bool,
) -> Option<Vec<u8>> {
    for path in presentation_cache_paths(identity, hash, scopes) {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if scopes && metadata.len() > 4 * 1024 * 1024 {
            return None;
        }
        // A present but invalid new result must not resurrect an older result.
        return std::fs::read(path).ok();
    }
    None
}

#[cfg(test)]
mod tests;
