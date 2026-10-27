//! Bounded release-note loading, independent of installer selection/download.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseNote {
    pub(super) version: String,
    pub(super) notes: String,
}

#[derive(Deserialize)]
struct Entry {
    tag_name: String,
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

fn select(
    entries: Vec<Entry>,
    current: &str,
    target: &UpdateRelease,
) -> Result<Vec<ReleaseNote>, String> {
    let current = AppVersion::parse(current)?;
    let latest = AppVersion::parse(&target.version)?;
    let mut versions = BTreeMap::new();
    for entry in entries {
        if entry.draft || entry.prerelease {
            continue;
        }
        let Ok(version) =
            AppVersion::parse(entry.tag_name.strip_prefix('v').unwrap_or(&entry.tag_name))
        else {
            continue;
        };
        if version > current && version <= latest {
            versions.entry(version).or_insert(ReleaseNote {
                version: version.to_string(),
                notes: entry.body.unwrap_or_default(),
            });
        }
    }
    if latest > current {
        // The validated update response is authoritative for its own notes.
        versions.insert(
            latest,
            ReleaseNote {
                version: latest.to_string(),
                notes: target.notes.clone(),
            },
        );
    }
    Ok(versions.into_values().rev().collect())
}

async fn fetch_entries(client: &reqwest::Client, url: &str) -> Result<Vec<Entry>, String> {
    const BUDGET: usize = 4 * 1024 * 1024;
    let mut retained = 0;
    let mut entries = Vec::new();
    for page in 1..=10 {
        let mut response = client
            .get(url)
            .query(&[("page", page), ("per_page", 50)])
            .header(USER_AGENT, format!("Torto/{}", env!("CARGO_PKG_VERSION")))
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            retained += chunk.len();
            if retained > BUDGET {
                return Err("Release notes exceed the history limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let batch: Vec<Entry> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let done = batch.len() < 50;
        entries.extend(batch);
        if done {
            return Ok(entries);
        }
    }
    Err("Release history exceeds the page limit".into())
}

async fn with_fallback<F, Fut>(mut fetch: F) -> Result<Vec<Entry>, String>
where
    F: FnMut(&'static str) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Entry>, String>>,
{
    match fetch("https://api.github.com/repos/TortoTech/torto/releases").await {
        Ok(entries) => Ok(entries),
        Err(github) => fetch("https://gitee.com/api/v5/repos/TortoTech/torto/releases")
            .await
            .map_err(|gitee| format!("{github}\n{gitee}")),
    }
}

pub(super) async fn fetch(
    current: &str,
    target: &UpdateRelease,
) -> Result<Vec<ReleaseNote>, String> {
    let client = crate::http::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(4))
        .build()
        .map_err(|e| e.to_string())?;
    // A slow mirror cannot hold a history job indefinitely across all pages.
    let entries = tokio::time::timeout(
        Duration::from_secs(25),
        with_fallback(|url| fetch_entries(&client, url)),
    )
    .await
    .map_err(|_| "Release history timed out".to_string())??;
    select(entries, current, target)
}

pub(super) fn display_notes(markdown: &str, language: AppLanguage) -> String {
    super::localized_release_notes(markdown, language)
        .lines()
        .filter(|line| {
            let value = line
                .trim()
                .trim_start_matches(['*', '-', '#', ' '])
                .to_lowercase();
            let footer = value.starts_with("full changelog")
                || value.starts_with("[full changelog]")
                || value.starts_with("完整更新")
                || value.starts_with("[完整更新");
            !(footer && (value.contains("](") || value.contains("https://")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_selects_stable_range_deduplicates_and_orders_semantically() {
        let entries = serde_json::from_str(
            r#"[
          {"tag_name":"v0.9.0","body":"nine"},
          {"tag_name":"v0.10.0","body":"ten"},
          {"tag_name":"0.9.0","body":"duplicate"},
          {"tag_name":"v0.8.0","body":"installed"},
          {"tag_name":"v0.11.0","body":"future"},
          {"tag_name":"v0.9.1","prerelease":true},
          {"tag_name":"v0.9.2","draft":true},
          {"tag_name":"v0.9.3-beta"}
        ]"#,
        )
        .unwrap();
        let target = UpdateRelease {
            version: "0.10.0".into(),
            notes: "authoritative".into(),
            asset: UpdateAsset {
                name: String::new(),
                url: String::new(),
                sha256: String::new(),
                size: 0,
            },
        };
        let selected = select(entries, "0.8.0", &target).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|r| r.version.as_str())
                .collect::<Vec<_>>(),
            ["0.10.0", "0.9.0"]
        );
        assert_eq!(selected[0].notes, "authoritative");
        assert_eq!(selected[1].notes, "nine");
    }

    #[test]
    fn display_filters_footer_after_localizing_without_removing_contributors() {
        let notes = "- Fix by [@author](https://github.com/author), [#1](https://github.com/TortoTech/torto/pull/1)\n**Full Changelog**: [v1...v2](https://github.com/TortoTech/torto/compare/v1...v2)\n<details>\n<summary>中文更新说明</summary>\n- 修复问题\n[Full Changelog](https://github.com/TortoTech/torto/compare/v1...v2)\n</details>";
        let english = display_notes(notes, AppLanguage::English);
        assert!(english.contains("@author") && english.contains("pull/1"));
        assert!(!english.contains("Changelog"));
        assert_eq!(
            display_notes(notes, AppLanguage::SimplifiedChinese),
            "- 修复问题"
        );
        assert!(notes.contains("Full Changelog"));
    }

    #[test]
    fn mirror_fallback_is_used_only_after_primary_failure() {
        let mut calls = Vec::new();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(with_fallback(|url| {
            calls.push(url);
            std::future::ready(if url.contains("github") {
                Err("unreachable".into())
            } else {
                Ok(vec![])
            })
        }));
        assert!(result.is_ok());
        assert_eq!(calls.len(), 2);
    }
}
