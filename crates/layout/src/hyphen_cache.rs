//! Engine-local measurements: font registrations belong to the same engine.
use crate::{PreparedHyphenGlyph, ReaderTypography};
use rebook_publication::{Rgba, TextStyle};

const CAPACITY: usize = 64;
const MAX_KEY_BYTES: usize = 16 * 1024;

#[derive(Default)]
pub(super) struct HyphenGlyphCache {
    entries: Vec<Entry>,
}

struct Entry {
    style: TextStyle,
    font_stack: String,
    typography: ReaderTypography,
    line_height: f32,
    foreground: Rgba,
    glyph: PreparedHyphenGlyph,
}

impl HyphenGlyphCache {
    pub(super) fn get(
        &mut self,
        style: TextStyle,
        font_stack: &str,
        typography: &ReaderTypography,
        line_height: f32,
        foreground: Rgba,
    ) -> Option<PreparedHyphenGlyph> {
        let index = self.entries.iter().position(|entry| {
            entry.style == style
                && entry.font_stack == font_stack
                && entry.typography == *typography
                && entry.line_height.to_bits() == line_height.to_bits()
                && entry.foreground == foreground
        })?;
        let entry = self.entries.remove(index);
        let glyph = entry.glyph.clone();
        self.entries.push(entry);
        Some(glyph)
    }

    pub(super) fn insert(
        &mut self,
        style: TextStyle,
        font_stack: &str,
        typography: &ReaderTypography,
        line_height: f32,
        foreground: Rgba,
        glyph: PreparedHyphenGlyph,
    ) {
        // Keep even unusually long user-configured font names bounded.
        let key_bytes = font_stack.len()
            + typography.default_cjk_font.len()
            + typography.serif_font.len()
            + typography.sans_serif_font.len()
            + typography.other_font.len()
            + typography.monospace_font.len()
            + typography.latin_cjk_font.as_ref().map_or(0, String::len)
            + typography
                .cjk_default_font
                .as_ref()
                .map_or(0, |font| font.family.len());
        if key_bytes > MAX_KEY_BYTES {
            return;
        }
        if self.entries.len() == CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(Entry {
            style,
            font_stack: font_stack.to_owned(),
            typography: typography.clone(),
            line_height,
            foreground,
            glyph,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn glyph() -> PreparedHyphenGlyph {
        PreparedHyphenGlyph {
            layout: Arc::new(crate::text_layout::Layout::new()),
            text: Arc::from("\u{2010}"),
            width: 5.0,
        }
    }

    #[test]
    fn measurements_require_all_font_and_paint_inputs_to_match() {
        let mut cache = HyphenGlyphCache::default();
        let style = TextStyle::default();
        let typography = ReaderTypography::default();
        cache.insert(style, "serif", &typography, 1.5, Rgba::BLACK, glyph());
        assert!(
            cache
                .get(style, "serif", &typography, 1.5, Rgba::BLACK)
                .is_some()
        );
        assert!(
            cache
                .get(style, "sans-serif", &typography, 1.5, Rgba::BLACK)
                .is_none()
        );
        assert!(
            cache
                .get(style, "serif", &typography, 1.8, Rgba::BLACK)
                .is_none()
        );
        assert!(
            cache
                .get(
                    style,
                    "serif",
                    &typography,
                    1.5,
                    Rgba {
                        alpha: 64,
                        ..Rgba::BLACK
                    }
                )
                .is_none()
        );
        assert!(
            cache
                .get(
                    TextStyle {
                        italic: true,
                        ..style
                    },
                    "serif",
                    &typography,
                    1.5,
                    Rgba::BLACK
                )
                .is_none()
        );
        let mut changed = typography.clone();
        changed.font_weight = 700;
        assert!(
            cache
                .get(style, "serif", &changed, 1.5, Rgba::BLACK)
                .is_none()
        );
        changed = typography.clone();
        changed.minimum_font_size += 1.0;
        assert!(
            cache
                .get(style, "serif", &changed, 1.5, Rgba::BLACK)
                .is_none()
        );
        changed = typography.clone();
        changed.serif_font = "different family".into();
        assert!(
            cache
                .get(style, "serif", &changed, 1.5, Rgba::BLACK)
                .is_none()
        );
    }

    #[test]
    fn cache_evicts_least_recently_used_entries_and_rejects_oversized_keys() {
        let mut cache = HyphenGlyphCache::default();
        let style = TextStyle::default();
        let typography = ReaderTypography::default();
        for index in 0..CAPACITY {
            cache.insert(
                style,
                &index.to_string(),
                &typography,
                1.5,
                Rgba::BLACK,
                glyph(),
            );
        }
        assert!(
            cache
                .get(style, "0", &typography, 1.5, Rgba::BLACK)
                .is_some()
        );
        cache.insert(style, "next", &typography, 1.5, Rgba::BLACK, glyph());
        assert_eq!(cache.entries.len(), CAPACITY);
        assert!(
            cache
                .get(style, "0", &typography, 1.5, Rgba::BLACK)
                .is_some()
        );
        assert!(
            cache
                .get(style, "1", &typography, 1.5, Rgba::BLACK)
                .is_none()
        );
        let huge = "x".repeat(MAX_KEY_BYTES + 1);
        cache.insert(style, &huge, &typography, 1.5, Rgba::BLACK, glyph());
        assert_eq!(cache.entries.len(), CAPACITY);
        assert!(
            cache
                .get(style, &huge, &typography, 1.5, Rgba::BLACK)
                .is_none()
        );
    }
}
