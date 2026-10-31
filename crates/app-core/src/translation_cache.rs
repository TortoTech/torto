//! Validated translation batches, keyed by source and model configuration.
//! API keys never participate in the persisted record.
use crate::plugins::{BlockTranslation, PluginSettings, TranslationBlockInput};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
const LIMIT: usize = 4 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Entry {
    input: String,
    text: String,
}
#[derive(Serialize, Deserialize)]
struct Stored {
    version: u8,
    entries: BTreeMap<String, Entry>,
}
pub struct TranslationCache {
    path: PathBuf,
    inputs: BTreeMap<String, (usize, Option<usize>, String)>,
    entries: BTreeMap<String, Entry>,
}
fn key(block: usize, segment: Option<usize>) -> String {
    format!(
        "{block}:{}",
        segment.map_or_else(|| "whole".into(), |s| s.to_string())
    )
}
pub fn configuration_key(settings: &PluginSettings) -> io::Result<String> {
    let provider = settings
        .providers
        .iter()
        .find(|p| p.id == settings.translation_provider);
    let bytes = serde_json::to_vec(&(
        &settings.target_language,
        &settings.translation_provider,
        &settings.translation_model,
        &settings.translation_reasoning_effort,
        settings.expert_translation,
        provider.map(|p| (&p.base_url, &p.kind)),
    ))
    .map_err(io::Error::other)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
#[derive(Serialize)]
struct StoredRef<'a> {
    version: u8,
    entries: &'a BTreeMap<String, Entry>,
}
impl TranslationCache {
    pub fn open(
        book: &str,
        section: usize,
        settings: &PluginSettings,
        inputs: &[(TranslationBlockInput, rebook_publication::SourceRange)],
    ) -> io::Result<Self> {
        let root = crate::environment::paths()
            .ok_or_else(|| io::Error::other("Core environment unavailable"))?
            .cache;
        Self::open_at(
            &root,
            book,
            section,
            settings,
            inputs.iter().map(|(input, _)| input),
        )
    }
    pub fn open_text_inputs(
        book: &str,
        settings: &PluginSettings,
        inputs: &[TranslationBlockInput],
    ) -> io::Result<Self> {
        let root = crate::environment::paths()
            .ok_or_else(|| io::Error::other("Core environment unavailable"))?
            .cache;
        Self::open_at(&root, book, usize::MAX, settings, inputs.iter())
    }
    fn open_at<'a>(
        root: &Path,
        book: &str,
        section: usize,
        settings: &PluginSettings,
        inputs: impl Iterator<Item = &'a TranslationBlockInput>,
    ) -> io::Result<Self> {
        let inputs = inputs
            .map(|input| {
                (
                    key(input.block_index, input.segment_index),
                    (
                        input.block_index,
                        input.segment_index,
                        blake3::hash(input.text.as_bytes()).to_hex().to_string(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let identity =
            serde_json::to_vec(&(1u8, book, section, &inputs, configuration_key(settings)?))
                .map_err(io::Error::other)?;
        let path = root
            .join("translations/v1")
            .join(crate::persistence::safe_book_id(book))
            .join(format!("{}.json", blake3::hash(&identity).to_hex()));
        let mut entries = BTreeMap::new();
        if let Ok(file) = fs::File::open(&path) {
            let mut bytes = Vec::new();
            if file.take(LIMIT as u64 + 1).read_to_end(&mut bytes).is_ok() && bytes.len() <= LIMIT {
                if let Ok(stored) = serde_json::from_slice::<Stored>(&bytes)
                    && stored.version == 1
                    && stored.entries.len() <= 4096
                {
                    entries = stored
                        .entries
                        .into_iter()
                        .filter(|(key, entry)| {
                            inputs
                                .get(key)
                                .is_some_and(|(_, _, input)| *input == entry.input)
                        })
                        .collect();
                }
            }
        }
        Ok(Self {
            path,
            inputs,
            entries,
        })
    }
    pub fn load(&self) -> Vec<BlockTranslation> {
        self.entries
            .iter()
            .filter_map(|(key, entry)| {
                self.inputs
                    .get(key)
                    .map(|(block, segment, _)| BlockTranslation {
                        block_index: *block,
                        segment_index: *segment,
                        text: entry.text.clone(),
                    })
            })
            .collect()
    }
    pub fn store(&mut self, batch: &[BlockTranslation]) -> io::Result<()> {
        for translated in batch {
            let key = key(translated.block_index, translated.segment_index);
            if let Some((_, _, input)) = self.inputs.get(&key) {
                self.entries.insert(
                    key,
                    Entry {
                        input: input.clone(),
                        text: translated.text.clone(),
                    },
                );
            }
        }
        let mut retained = self
            .entries
            .values()
            .map(|entry| entry.input.len() + entry.text.len())
            .sum::<usize>();
        while self.entries.len() > 4096 || retained > LIMIT / 2 {
            let Some(first) = self.entries.keys().next().cloned() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&first) {
                retained -= entry.input.len() + entry.text.len();
            }
        }
        let bytes = serde_json::to_vec(&StoredRef {
            version: 1,
            entries: &self.entries,
        })
        .map_err(io::Error::other)?;
        if bytes.len() > LIMIT || self.entries.len() > 4096 {
            return Ok(());
        }
        crate::persistence::write_bytes_atomic(&self.path, &bytes)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_reuses_only_matching_source_language_and_model() {
        let root =
            std::env::temp_dir().join(format!("torto-translations-{}", uuid::Uuid::new_v4()));
        let mut settings = PluginSettings::default();
        settings.target_language = "简体中文".into();
        settings.translation_model = "model-a".into();
        let input = TranslationBlockInput {
            block_index: 2,
            segment_index: None,
            text: "Source paragraph".into(),
        };
        let mut cache =
            TranslationCache::open_at(&root, "book", 0, &settings, std::iter::once(&input))
                .unwrap();
        cache
            .store(&[BlockTranslation {
                block_index: 2,
                segment_index: None,
                text: "已翻译".into(),
            }])
            .unwrap();
        let restored =
            TranslationCache::open_at(&root, "book", 0, &settings, std::iter::once(&input))
                .unwrap();
        assert_eq!(restored.load()[0].text, "已翻译");
        settings.target_language = "English".into();
        assert!(
            TranslationCache::open_at(&root, "book", 0, &settings, std::iter::once(&input))
                .unwrap()
                .load()
                .is_empty()
        );
        settings.target_language = "简体中文".into();
        settings.translation_model = "model-b".into();
        assert!(
            TranslationCache::open_at(&root, "book", 0, &settings, std::iter::once(&input))
                .unwrap()
                .load()
                .is_empty()
        );
        settings.translation_model = "model-a".into();
        let changed = TranslationBlockInput {
            text: "Changed source".into(),
            ..input.clone()
        };
        assert!(
            TranslationCache::open_at(&root, "book", 0, &settings, std::iter::once(&changed))
                .unwrap()
                .load()
                .is_empty()
        );
        fs::write(&cache.path, b"corrupt cache").unwrap();
        assert!(
            TranslationCache::open_at(&root, "book", 0, &settings, std::iter::once(&input))
                .unwrap()
                .load()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
