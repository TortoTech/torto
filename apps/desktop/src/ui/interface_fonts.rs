use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use egui::{FontData, FontDefinitions, FontFamily};
use skrifa::MetadataProvider as _;

use crate::preferences::{AppLanguage, InterfaceTypography, SYSTEM_INTERFACE_FONT};

fn state_id() -> egui::Id {
    egui::Id::new("interface-font-fallbacks")
}

struct Face {
    source: fontdb::Source,
    index: u32,
}

impl Face {
    fn query(database: &fontdb::Database, family: &str, weight: fontdb::Weight) -> Option<Self> {
        let id = database.query(&fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight,
            ..Default::default()
        })?;
        let (source, index) = database.face_source(id)?;
        // Keep fallback file locations, not all of the discovery database's
        // font mappings. Font bytes are read only when a face is needed.
        let source = match source {
            fontdb::Source::SharedFile(path, _) => fontdb::Source::File(path),
            source => source,
        };
        Some(Self { source, index })
    }

    fn data(&self) -> Option<FontData> {
        let bytes = match &self.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                std::fs::read(path).ok()?
            }
            fontdb::Source::Binary(bytes) => bytes.as_ref().as_ref().to_vec(),
        };
        skrifa::FontRef::from_index(&bytes, self.index).ok()?;
        let mut data = FontData::from_owned(bytes);
        data.index = self.index;
        Some(data)
    }
}

struct Candidate {
    family: String,
    regular: Face,
    bold: Option<Face>,
}

struct InterfaceFonts {
    configuration: (String, AppLanguage),
    definitions: FontDefinitions,
    candidates: Vec<Candidate>,
    loaded: HashSet<String>,
    regular: Vec<String>,
    bold: Vec<String>,
    checked: HashMap<FontFamily, HashSet<char>>,
}

impl InterfaceFonts {
    fn new(typography: &InterfaceTypography, language: AppLanguage) -> Self {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let mut families = Vec::new();
        if typography.font_family != SYSTEM_INTERFACE_FONT {
            families.push(typography.font_family.clone());
        }
        families.extend(
            super::system_ui_font_candidates(language)
                .iter()
                .map(ToString::to_string),
        );
        let mut unique = HashSet::new();
        let candidates = families
            .into_iter()
            .filter(|family| unique.insert(family.clone()))
            .filter_map(|family| {
                Some(Candidate {
                    regular: Face::query(&database, &family, fontdb::Weight::NORMAL)?,
                    bold: Face::query(&database, &family, fontdb::Weight::BOLD),
                    family,
                })
            })
            .collect();
        let mut state = Self {
            configuration: (typography.font_family.clone(), language.resolved()),
            definitions: FontDefinitions::default(),
            candidates,
            loaded: HashSet::new(),
            regular: Vec::new(),
            bold: Vec::new(),
            checked: HashMap::new(),
        };
        for index in 0..state.candidates.len() {
            if let Some(data) = state.candidates[index].regular.data() {
                state.add_candidate(index, data);
                break;
            }
        }
        state.rebuild_families();
        state
    }

    fn add_candidate(&mut self, index: usize, data: FontData) {
        let candidate = &self.candidates[index];
        let regular = format!("system-ui-regular-{}", candidate.family);
        self.definitions
            .font_data
            .insert(regular.clone(), Arc::new(data));
        self.regular.push(regular);
        if let Some(data) = candidate.bold.as_ref().and_then(Face::data) {
            let bold = format!("system-ui-bold-{}", candidate.family);
            self.definitions
                .font_data
                .insert(bold.clone(), Arc::new(data));
            self.bold.push(bold);
        }
        self.loaded.insert(candidate.family.clone());
    }

    fn rebuild_families(&mut self) {
        let defaults = FontDefinitions::default();
        let mut proportional = self.regular.clone();
        proportional.extend(defaults.families[&FontFamily::Proportional].clone());
        if self.definitions.font_data.contains_key("reader-cjk") {
            proportional.push("reader-cjk".into());
        }
        let mut bold = self.bold.clone();
        bold.extend(proportional.clone());
        self.definitions
            .families
            .insert(FontFamily::Proportional, proportional);
        self.definitions.families.insert(
            FontFamily::Name(egui_commonmark_backend::STRONG_FONT_FAMILY.into()),
            bold,
        );
        let mut monospace = defaults.families[&FontFamily::Monospace].clone();
        monospace.extend(self.regular.clone());
        if self.definitions.font_data.contains_key("reader-cjk") {
            monospace.push("reader-cjk".into());
        }
        self.definitions
            .families
            .insert(FontFamily::Monospace, monospace);
    }

    fn resolve(&mut self, ctx: &egui::Context, shapes: &[egui::epaint::ClippedShape]) -> bool {
        let mut missing = HashSet::new();
        ctx.fonts_mut(|fonts| {
            // Settings can queue a new font configuration during a pass. Wait
            // until egui applies it rather than probing the previous fonts and
            // needlessly adding fallbacks to the new configuration.
            if fonts.definitions().families != self.definitions.families {
                return;
            }
            for shape in shapes {
                collect_missing(&shape.shape, &mut self.checked, &mut missing, fonts);
            }
        });
        if missing.is_empty() {
            return false;
        }
        let mut changed = false;
        for index in 0..self.candidates.len() {
            if self.loaded.contains(&self.candidates[index].family) {
                continue;
            }
            let Some(data) = self.candidates[index].regular.data() else {
                continue;
            };
            let font = skrifa::FontRef::from_index(&data.font, data.index).unwrap();
            let charmap = font.charmap();
            let supports = |character: &char| {
                charmap
                    .map(*character)
                    .is_some_and(|glyph| glyph != skrifa::GlyphId::NOTDEF)
            };
            if !missing.iter().any(supports) {
                continue;
            }
            missing.retain(|character| !supports(character));
            self.add_candidate(index, data);
            changed = true;
            if missing.is_empty() {
                break;
            }
        }
        if !missing.is_empty() && !self.definitions.font_data.contains_key("reader-cjk") {
            let data = FontData::from_static(crate::fonts::cjk_font_bytes());
            if let Ok(font) = skrifa::FontRef::from_index(&data.font, data.index)
                && missing.iter().any(|character| {
                    font.charmap()
                        .map(*character)
                        .is_some_and(|glyph| glyph != skrifa::GlyphId::NOTDEF)
                })
            {
                self.definitions
                    .font_data
                    .insert("reader-cjk".into(), Arc::new(data));
                changed = true;
            }
        }
        if changed {
            self.rebuild_families();
        }
        changed
    }
}

fn collect_missing(
    shape: &egui::Shape,
    checked: &mut HashMap<FontFamily, HashSet<char>>,
    missing: &mut HashSet<char>,
    fonts: &mut egui::epaint::text::FontsView<'_>,
) {
    match shape {
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_missing(shape, checked, missing, fonts);
            }
        }
        egui::Shape::Text(text) => {
            for section in &text.galley.job.sections {
                let font = &section.format.font_id;
                let seen = checked.entry(font.family.clone()).or_default();
                for character in text.galley.job.text
                    [section.byte_range.start.0..section.byte_range.end.0]
                    .chars()
                {
                    if !character.is_control()
                        && seen.insert(character)
                        && !fonts.has_glyph(font, character)
                    {
                        missing.insert(character);
                    }
                }
            }
        }
        _ => {}
    }
}

pub(super) fn configure(
    ctx: &egui::Context,
    typography: &InterfaceTypography,
    language: AppLanguage,
) {
    let current = ctx.data_mut(|data| data.get_temp::<Arc<Mutex<InterfaceFonts>>>(state_id()));
    if let Some(current) = current
        && current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .configuration
            == (typography.font_family.clone(), language.resolved())
    {
        return;
    }
    let state = InterfaceFonts::new(typography, language);
    ctx.set_fonts(state.definitions.clone());
    ctx.data_mut(|data| data.insert_temp(state_id(), Arc::new(Mutex::new(state))));
}

pub(crate) fn resolve(ctx: &egui::Context, shapes: &[egui::epaint::ClippedShape]) -> bool {
    let state = ctx.data_mut(|data| data.get_temp::<Arc<Mutex<InterfaceFonts>>>(state_id()));
    let Some(state) = state else {
        return false;
    };
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !state.resolve(ctx, shapes) {
        return false;
    }
    ctx.set_fonts(state.definitions.clone());
    ctx.request_repaint();
    true
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    fn draw(ctx: &egui::Context, text: &str) -> egui::FullOutput {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label(text);
            ui.label(egui::RichText::new(text).family(FontFamily::Name(
                egui_commonmark_backend::STRONG_FONT_FAMILY.into(),
            )));
            ui.label(egui::RichText::new(text).monospace());
        });
        output.textures_delta.clear();
        output
    }

    fn loaded(ctx: &egui::Context) -> Vec<String> {
        ctx.fonts_mut(|fonts| fonts.definitions().font_data.keys().cloned().collect())
    }

    #[test]
    fn unsupported_characters_do_not_load_fonts_that_cannot_display_them() {
        let ctx = egui::Context::default();
        configure(&ctx, &InterfaceTypography::default(), AppLanguage::English);
        let output = draw(&ctx, "A?\u{10ffff}");
        ctx.fonts_mut(|fonts| {
            let font = egui::FontId::proportional(14.0);
            assert!(fonts.has_glyphs(&font, "A?"));
            assert!(!fonts.has_glyph(&font, '\u{10ffff}'));
        });
        assert!(!resolve(&ctx, &output.shapes));
        assert!(
            !loaded(&ctx)
                .iter()
                .any(|name| name.contains("YaHei") || name == "reader-cjk")
        );
        output.drop_without_applying_deltas();
    }

    #[test]
    fn chinese_system_ui_does_not_load_redundant_latin_or_embedded_cjk_fonts() {
        let ctx = egui::Context::default();
        configure(
            &ctx,
            &InterfaceTypography::default(),
            AppLanguage::SimplifiedChinese,
        );
        let output = draw(&ctx, "AI排版 Settings");
        assert!(!resolve(&ctx, &output.shapes));
        let names = loaded(&ctx);
        assert!(
            names
                .iter()
                .any(|name| name == "system-ui-regular-Microsoft YaHei UI")
        );
        assert!(
            !names
                .iter()
                .any(|name| name.contains("Segoe UI") || name == "reader-cjk")
        );
        output.drop_without_applying_deltas();
    }

    #[test]
    fn english_ui_loads_chinese_fallback_only_after_chinese_text_is_displayed() {
        let ctx = egui::Context::default();
        configure(&ctx, &InterfaceTypography::default(), AppLanguage::English);
        let output = draw(&ctx, "Library Settings");
        assert!(!resolve(&ctx, &output.shapes));
        assert!(
            !loaded(&ctx)
                .iter()
                .any(|name| name.contains("YaHei") || name == "reader-cjk")
        );
        output.drop_without_applying_deltas();
        let output = draw(&ctx, "中文书名");
        assert!(resolve(&ctx, &output.shapes));
        output.drop_without_applying_deltas();
        let output = draw(&ctx, "中文书名");
        ctx.fonts_mut(|fonts| {
            for family in [
                FontFamily::Proportional,
                FontFamily::Monospace,
                FontFamily::Name(egui_commonmark_backend::STRONG_FONT_FAMILY.into()),
            ] {
                assert!(fonts.has_glyphs(&egui::FontId::new(14.0, family), "中文书名"));
            }
        });
        assert!(!resolve(&ctx, &output.shapes));
        assert!(
            loaded(&ctx)
                .iter()
                .any(|name| name == "system-ui-regular-Microsoft YaHei UI")
        );
        assert!(!loaded(&ctx).contains(&"reader-cjk".into()));
        output.drop_without_applying_deltas();
        // Changing language/settings must release the previously loaded fallback.
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            configure(
                &ctx,
                &InterfaceTypography::default(),
                AppLanguage::SimplifiedChinese,
            );
            ui.label("设置");
        });
        output.textures_delta.clear();
        assert!(!resolve(&ctx, &output.shapes));
        output.drop_without_applying_deltas();
        let output = draw(&ctx, "设置");
        assert!(
            !loaded(&ctx)
                .iter()
                .any(|name| name.contains("Segoe") || name == "reader-cjk")
        );
        output.drop_without_applying_deltas();
        configure(&ctx, &InterfaceTypography::default(), AppLanguage::English);
        let output = draw(&ctx, "Settings");
        assert!(!loaded(&ctx).iter().any(|name| name.contains("YaHei")));
        output.drop_without_applying_deltas();
    }

    #[test]
    fn custom_font_takes_priority_and_missing_font_uses_language_default() {
        let ctx = egui::Context::default();
        let mut typography = InterfaceTypography::default();
        typography.font_family = "Consolas".into();
        configure(&ctx, &typography, AppLanguage::SimplifiedChinese);
        let output = draw(&ctx, "Settings");
        ctx.fonts_mut(|fonts| {
            assert_eq!(
                fonts.definitions().families[&FontFamily::Proportional][0],
                "system-ui-regular-Consolas"
            )
        });
        assert!(!resolve(&ctx, &output.shapes));
        output.drop_without_applying_deltas();
        let output = draw(&ctx, "字体设置");
        assert!(resolve(&ctx, &output.shapes));
        output.drop_without_applying_deltas();
        let output = draw(&ctx, "字体设置");
        ctx.fonts_mut(|fonts| {
            assert_eq!(
                fonts.definitions().families[&FontFamily::Proportional][0],
                "system-ui-regular-Consolas"
            )
        });
        assert!(!resolve(&ctx, &output.shapes));
        output.drop_without_applying_deltas();
        typography.font_family = "Torto nonexistent test font".into();
        configure(&ctx, &typography, AppLanguage::SimplifiedChinese);
        let output = draw(&ctx, "设置");
        ctx.fonts_mut(|fonts| {
            assert_eq!(
                fonts.definitions().families[&FontFamily::Proportional][0],
                "system-ui-regular-Microsoft YaHei UI"
            )
        });
        output.drop_without_applying_deltas();
    }
}
