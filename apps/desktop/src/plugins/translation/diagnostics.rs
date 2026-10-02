use super::{Block, Inline, TextBlock, math_placeholder_indices, visit_note_text_blocks_mut};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
struct Seen(VecDeque<String>);
impl Seen {
    fn accept(&mut self, key: String) -> bool {
        if self.0.contains(&key) {
            return false;
        }
        if self.0.len() >= 512 {
            self.0.pop_front();
        }
        self.0.push_back(key);
        true
    }
}

pub(super) fn event(name: &str, details: Value) {
    // Unit tests must not contaminate diagnostics from actual reading sessions.
    if cfg!(test) || !cfg!(debug_assertions) {
        return;
    }
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    let thread = std::thread::current();
    let mut record = details;
    record["thread"] = json!(thread.name().unwrap_or("unnamed"));
    let key = fingerprint(&format!("{name}{record}"));
    let Ok(mut seen) = SEEN.get_or_init(Default::default).lock() else {
        return;
    };
    if !seen.accept(key) {
        return;
    }
    drop(seen);
    record["thread_id"] = json!(format!("{:?}", thread.id()));
    record["pid"] = json!(std::process::id());
    record["version"] = json!(env!("CARGO_PKG_VERSION"));
    crate::plugins::semantic_layout::local_translation_event(name, record);
}

pub(super) fn fingerprint(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub(super) fn translated(text: &str) -> Value {
    json!({"hash":fingerprint(text), "chars":text.chars().count(),
        "preview":text.chars().take(240).collect::<String>(),
        "placeholder_ids":math_placeholder_indices(text).ok(),
        "legacy_tags":text.contains("<torto-math-")})
}

pub(super) fn inlines(content: &[Inline]) -> Value {
    let encoded = serde_json::to_string(content).unwrap_or_default();
    let formulas: Vec<_> = content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Math(run) => Some(run.latex.chars().take(120).collect::<String>()),
            _ => None,
        })
        .collect();
    let images: Vec<_> = content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Image(run) => Some(serde_json::to_value(run).unwrap_or_default()),
            _ => None,
        })
        .collect();
    let text: String = content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text(run) => Some(run.text.as_str()),
            _ => None,
        })
        .collect();
    json!({"hash":fingerprint(&encoded), "text_preview":text.chars().take(200).collect::<String>(),
        "math_count":formulas.len(), "math":formulas.into_iter().take(12).collect::<Vec<_>>(),
        "image_count":images.len(), "images":images.into_iter().take(4).collect::<Vec<_>>()})
}

pub(super) fn block(block: &Block) -> Value {
    let mut block = block.clone();
    let mut children = Vec::new();
    visit_note_text_blocks_mut(&mut block, &mut 0, &mut |index, text: &mut TextBlock| {
        if children.len() < 12 {
            children.push(
                json!({"segment":index,"source":text.source,"content":inlines(&text.content)}),
            );
        }
    });
    json!(children)
}

pub(super) fn formula_mismatch(block: &Block, translation: &super::StoredBlockTranslation) -> bool {
    if let Block::Text(text) = block {
        return translation.whole.as_ref().is_some_and(|translated| {
            let count = text
                .content
                .iter()
                .filter(|inline| matches!(inline, Inline::Math(_)))
                .count();
            super::validate_math_placeholder_count(translated, count).is_err()
        });
    }
    let mut block = block.clone();
    let whole = matches!(block, Block::Text(_));
    let mut mismatch = false;
    visit_note_text_blocks_mut(&mut block, &mut 0, &mut |index, text| {
        let translated = if whole {
            translation.whole.as_ref()
        } else {
            translation.segments.get(&index)
        };
        if let Some(translated) = translated {
            let count = text
                .content
                .iter()
                .filter(|inline| matches!(inline, Inline::Math(_)))
                .count();
            mismatch |= super::validate_math_placeholder_count(translated, count).is_err();
        }
    });
    mismatch
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_are_deduplicated_bounded_and_accept_old_placeholder_ids() {
        let mut seen = Seen::default();
        assert!(seen.accept("same".into()));
        assert!(!seen.accept("same".into()));
        for index in 0..600 {
            assert!(seen.accept(index.to_string()));
        }
        assert_eq!(seen.0.len(), 512);
        assert_eq!(translated("<torto-math-0/>")["placeholder_ids"], json!([0]));
        assert_eq!(translated("<t-math-bad/>")["placeholder_ids"], Value::Null);
        assert_ne!(fingerprint("one"), fingerprint("two"));
    }

    #[test]
    fn prepared_formula_diagnostics_only_flag_actual_restore_mismatches() {
        let mut translation = super::super::StoredBlockTranslation::default();
        translation.whole = Some("Value <t-math-0/>".into());
        let original = Block::Text(TextBlock {
            kind: super::super::TextBlockKind::Paragraph,
            style: Default::default(),
            source: None,
            content: vec![Inline::Text(super::super::TextRun {
                text: "Value".into(),
                style: Default::default(),
                link: None,
            })],
        });
        assert!(formula_mismatch(&original, &translation));
        let mut prepared = original.clone();
        if let Block::Text(block) = &mut prepared {
            block
                .content
                .push(Inline::Math(rebook_publication::MathRun {
                    original: None,
                    latex: "x".into(),
                    display: false,
                    size_scale: 1.0,
                }));
        }
        assert!(!formula_mismatch(&prepared, &translation));
        translation.whole = Some("Value".into());
        assert!(formula_mismatch(&prepared, &translation));
    }
}
