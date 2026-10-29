use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use egui::{FontData, FontDefinitions, FontFamily};

use crate::preferences::{AppLanguage, InterfaceTypography, SYSTEM_INTERFACE_FONT};

fn state_id() -> egui::Id {
    egui::Id::unique("interface-font-fallbacks")
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

    fn data(&self, files: &mut HashMap<std::path::PathBuf, Arc<Vec<u8>>>) -> Option<FontData> {
        let bytes = match &self.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                if let Some(bytes) = files.get(path) {
                    bytes.clone()
                } else {
                    let bytes = Arc::new(std::fs::read(path).ok()?);
                    files.insert(path.clone(), bytes.clone());
                    bytes
                }
            }
            fontdb::Source::Binary(bytes) => Arc::new(bytes.as_ref().as_ref().to_vec()),
        };
        skrifa::FontRef::from_index(&bytes, self.index).ok()?;
        Some(FontData::from_blob(bytes, self.index))
    }
}

mod system;
use system::SystemFonts;

struct InterfaceFonts {
    needs_apply: bool,
    definitions: FontDefinitions,
}

struct FontState {
    configuration: (String, String, AppLanguage),
    ready: Mutex<Option<InterfaceFonts>>,
    provider: Arc<SystemFonts>,
    #[cfg(test)]
    completed: std::sync::Condvar,
}

impl InterfaceFonts {
    fn new(
        typography: &InterfaceTypography,
        monospace_family: &str,
        language: AppLanguage,
    ) -> Self {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let mut mono_families = vec![
            monospace_family.to_owned(),
            rebook_layout::default_monospace_font().to_owned(),
            "Consolas".into(),
            "Menlo".into(),
            "Courier New".into(),
            "DejaVu Sans Mono".into(),
            "Liberation Mono".into(),
        ];
        mono_families.extend(
            database
                .faces()
                .filter(|face| face.monospaced)
                .filter_map(|face| face.families.first().map(|(name, _)| name.clone())),
        );
        // Configured system faces only. No built-in Latin, mono or emoji fonts.
        let mut definitions = FontDefinitions::empty();
        let mut files = HashMap::new();
        let mut unique = HashSet::new();
        let mono = mono_families
            .into_iter()
            .filter(|family| unique.insert(family.clone()))
            .find_map(|family| {
                let data =
                    Face::query(&database, &family, fontdb::Weight::NORMAL)?.data(&mut files)?;
                let name = format!("system-ui-monospace-{family}");
                definitions.font_data.insert(name.clone(), Arc::new(data));
                Some(name)
            });
        let mut families = Vec::new();
        if typography.font_family != SYSTEM_INTERFACE_FONT {
            families.push(typography.font_family.clone());
        }
        families.extend(
            super::system_ui_font_candidates(language)
                .iter()
                .map(ToString::to_string),
        );
        let selected = families.into_iter().find_map(|family| {
            let data = Face::query(&database, &family, fontdb::Weight::NORMAL)?.data(&mut files)?;
            Some((family, data))
        });
        let mut regular = Vec::new();
        let mut bold = Vec::new();
        if let Some((family, data)) = selected {
            let name = format!("system-ui-regular-{family}");
            definitions.font_data.insert(name.clone(), Arc::new(data));
            regular.push(name);
            if let Some(data) = Face::query(&database, &family, fontdb::Weight::BOLD)
                .and_then(|face| face.data(&mut files))
            {
                let name = format!("system-ui-bold-{family}");
                definitions.font_data.insert(name.clone(), Arc::new(data));
                bold.push(name);
            }
        }
        bold.extend(regular.clone());
        let mut monospace = mono.into_iter().collect::<Vec<_>>();
        monospace.extend(regular.clone());
        definitions
            .families
            .insert(FontFamily::Proportional, regular);
        definitions
            .families
            .insert(FontFamily::Monospace, monospace);
        definitions.families.insert(
            FontFamily::Name(egui_commonmark_backend::STRONG_FONT_FAMILY.into()),
            bold,
        );
        Self {
            needs_apply: true,
            definitions,
        }
    }
}

fn state(ctx: &egui::Context) -> Option<Arc<FontState>> {
    ctx.data(|data| data.get_temp::<Arc<FontState>>(state_id()))
}

pub(super) fn configure(
    ctx: &egui::Context,
    typography: &InterfaceTypography,
    monospace_family: &str,
    language: AppLanguage,
) {
    let configuration = (
        typography.font_family.clone(),
        monospace_family.into(),
        language.resolved(),
    );
    let current = state(ctx);
    if current
        .as_ref()
        .is_some_and(|current| current.configuration == configuration)
    {
        return;
    }
    if current.is_none() {
        // Until system faces arrive, egui 0.37 draws its missing-glyph marker.
        // Binding the Markdown family also makes first-frame headings safe.
        let mut bootstrap = FontDefinitions::empty();
        bootstrap.families.insert(
            FontFamily::Name(egui_commonmark_backend::STRONG_FONT_FAMILY.into()),
            vec![],
        );
        ctx.set_fonts(bootstrap);
    }
    let provider = SystemFonts::new(language);
    let state = Arc::new(FontState {
        configuration,
        ready: Mutex::new(None),
        provider: provider.clone(),
        #[cfg(test)]
        completed: std::sync::Condvar::new(),
    });
    ctx.data_mut(|data| data.insert_temp(state_id(), state.clone()));
    ctx.set_font_providers(vec![provider]);
    let wake = ctx.clone();
    let typography = typography.clone();
    let monospace_family = monospace_family.to_owned();
    std::thread::spawn(move || {
        let fonts = InterfaceFonts::new(&typography, &monospace_family, language);
        *state
            .ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fonts);
        #[cfg(test)]
        state.completed.notify_all();
        wake.request_repaint();
    });
}

pub(crate) fn resolve(ctx: &egui::Context, _shapes: &[egui::epaint::ClippedShape]) -> bool {
    let Some(state) = state(ctx) else {
        return false;
    };
    let mut changed = state.provider.advance(ctx);
    let mut ready = state
        .ready
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(fonts) = ready.as_mut()
        && fonts.needs_apply
    {
        fonts.needs_apply = false;
        ctx.set_fonts(fonts.definitions.clone());
        ctx.request_repaint();
        changed = true;
    }
    changed
}

pub(crate) fn resources_changed(ctx: &egui::Context) {
    if let Some(state) = state(ctx) {
        state.provider.resources_changed();
        ctx.request_repaint();
    }
}

#[cfg(test)]
pub(super) fn wait_for_monospace(ctx: &egui::Context) {
    let slot = state(ctx).unwrap();
    let (mut ready, _) = slot
        .completed
        .wait_timeout_while(
            slot.ready.lock().unwrap(),
            std::time::Duration::from_secs(10),
            |ready| ready.is_none(),
        )
        .unwrap();
    let fonts = ready.as_mut().expect("interface font discovery completed");
    fonts.needs_apply = false;
    ctx.set_fonts(fonts.definitions.clone());
}

#[cfg(test)]
mod monospace_tests {
    use super::*;

    fn selected(ctx: &egui::Context) -> String {
        ctx.data(|data| data.get_temp::<Arc<FontState>>(state_id()))
            .unwrap()
            .ready
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .definitions
            .families[&FontFamily::Monospace][0]
            .clone()
    }

    #[test]
    fn interface_uses_reader_monospace_and_reloads_after_selection_changes() {
        let ctx = egui::Context::default();
        let typography = InterfaceTypography::default();
        let default = rebook_layout::default_monospace_font();
        configure(&ctx, &typography, default, AppLanguage::English);
        wait_for_monospace(&ctx);
        assert_eq!(selected(&ctx), format!("system-ui-monospace-{default}"));

        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let other = database
            .faces()
            .filter(|face| face.monospaced)
            .flat_map(|face| &face.families)
            .map(|(name, _)| name)
            .find(|name| name.as_str() != default)
            .expect("another system monospace family");
        configure(&ctx, &typography, other, AppLanguage::English);
        wait_for_monospace(&ctx);
        assert_eq!(selected(&ctx), format!("system-ui-monospace-{other}"));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label(egui::RichText::new("let value = 123;").monospace());
        });
        ctx.fonts_mut(|fonts| {
            assert!(!fonts.definitions().font_data.contains_key("Hack"));
            assert!(fonts.has_glyphs(&egui::FontId::monospace(14.0), "let value = 123;"));
            assert!(
                !fonts
                    .definitions()
                    .font_data
                    .contains_key(&format!("system-ui-monospace-{default}"))
            );
        });
        output.textures_delta.clear();
    }

    #[test]
    fn unavailable_reader_monospace_uses_system_fallback() {
        let ctx = egui::Context::default();
        configure(
            &ctx,
            &InterfaceTypography::default(),
            "Missing Torto Test Mono",
            AppLanguage::English,
        );
        wait_for_monospace(&ctx);
        assert_eq!(
            selected(&ctx),
            format!(
                "system-ui-monospace-{}",
                rebook_layout::default_monospace_font()
            )
        );
    }
}

#[cfg(test)]
mod system_tests {
    use super::*;

    #[test]
    fn system_fallback_renders_cjk_and_color_emoji_without_embedded_fonts() {
        let ctx = egui::Context::default();
        let typography = InterfaceTypography {
            font_family: if cfg!(target_os = "macos") {
                "Arial"
            } else if cfg!(target_os = "windows") {
                "Segoe UI"
            } else {
                "DejaVu Sans"
            }
            .into(),
            ..Default::default()
        };
        configure(
            &ctx,
            &typography,
            rebook_layout::default_monospace_font(),
            AppLanguage::English,
        );
        // The first pass must also be safe while discovery is still pending.
        ctx.run_ui(Default::default(), |ui| {
            ui.label(egui::RichText::new("Starting").family(FontFamily::Name(
                egui_commonmark_backend::STRONG_FONT_FAMILY.into(),
            )));
        })
        .drop_without_applying_deltas();
        wait_for_monospace(&ctx);
        let provider = state(&ctx).unwrap().provider.clone();
        let mut colored = false;
        let mut covered = false;
        for _ in 0..12 {
            let output = ctx.run_ui(Default::default(), |ui| {
                ui.label("English 中文 😀 🐢");
            });
            for delta in output.textures_delta.set.values().flatten() {
                let egui::ImageData::Color(image) = &delta.image;
                colored |= image
                    .pixels
                    .iter()
                    .any(|pixel| pixel.r() != pixel.g() || pixel.g() != pixel.b());
            }
            ctx.fonts_mut(|fonts| {
                covered = fonts.has_glyphs(&egui::FontId::proportional(14.0), "English 中文 😀 🐢");
                let definitions = fonts.definitions();
                assert!(definitions.font_data.keys().all(|name| {
                    ![
                        "Ubuntu-Light",
                        "Hack",
                        "NotoEmoji-Regular",
                        "emoji-icon-font",
                    ]
                    .contains(&name.as_str())
                }));
                assert_eq!(
                    definitions.families[&FontFamily::Proportional][0],
                    format!("system-ui-regular-{}", typography.font_family)
                );
            });
            output.drop_without_applying_deltas();
            resolve(&ctx, &[]);
            provider.wait();
            resolve(&ctx, &[]);
            if covered && colored {
                break;
            }
        }
        assert!(covered, "system fonts cover Latin, CJK and Emoji");
        assert!(colored, "native color Emoji reach the egui texture atlas");
        // Once those fonts are loaded, repeated frames must not restart discovery.
        for _ in 0..3 {
            ctx.run_ui(Default::default(), |ui| {
                ui.label("English 中文 😀 🐢");
            })
            .drop_without_applying_deltas();
            assert!(!resolve(&ctx, &[]));
        }
    }

    #[test]
    fn file_backed_faces_share_font_bytes() {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let (source, index) = database
            .faces()
            .find_map(|face| {
                let (source, index) = database.face_source(face.id)?;
                matches!(
                    source,
                    fontdb::Source::File(_) | fontdb::Source::SharedFile(_, _)
                )
                .then_some((source, index))
            })
            .expect("a system font file");
        let regular = Face {
            source: source.clone(),
            index,
        };
        let other_index = database
            .faces()
            .find_map(|face| {
                let (other, index) = database.face_source(face.id)?;
                match (&source, other) {
                    (fontdb::Source::File(a), fontdb::Source::File(b)) if *a == b => Some(index),
                    _ => None,
                }
            })
            .unwrap_or(index);
        let other = Face {
            source,
            index: other_index,
        };
        let mut files = HashMap::new();
        let a = regular.data(&mut files).unwrap();
        let b = other.data(&mut files).unwrap();
        assert!(Arc::ptr_eq(&a.font, &b.font));
        assert_eq!(files.len(), 1);
        assert_eq!(a.index, index);
        assert_eq!(b.index, other_index);
    }
}
