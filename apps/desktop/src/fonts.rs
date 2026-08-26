use std::sync::Arc;

use rebook_layout::ReaderFontBlob;

const BITTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Bitter-wght.ttf");
const BITTER_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/Bitter-Italic-wght.ttf");
const ROBOTO_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Roboto-wdth-wght.ttf");
const ROBOTO_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/Roboto-Italic-wdth-wght.ttf");
pub(crate) fn cjk_font_bytes() -> &'static [u8] {
    rebook_formats::cjk_fallback_font_bytes()
}

pub fn embedded_reader_fonts() -> Arc<[ReaderFontBlob]> {
    [
        font_blob(BITTER_REGULAR),
        font_blob(BITTER_ITALIC),
        font_blob(ROBOTO_REGULAR),
        font_blob(ROBOTO_ITALIC),
        font_blob(cjk_font_bytes()),
    ]
    .into()
}

fn font_blob(bytes: &'static [u8]) -> ReaderFontBlob {
    ReaderFontBlob::from_static(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::LayoutEngine;

    #[test]
    fn embedded_font_assets_are_non_empty() {
        let fonts = embedded_reader_fonts();
        assert_eq!(fonts.len(), 5);
        assert!(fonts.iter().all(|font| !font.is_empty()));
    }

    #[test]
    fn embedded_fonts_register_with_reader_family_names() {
        let fonts = embedded_reader_fonts();
        let mut engine = LayoutEngine::with_fonts(fonts.iter().cloned());
        let mut families = engine.available_reader_font_families();
        families.include_configured(&rebook_layout::ReaderTypography::default());

        for expected in ["Bitter", "Roboto", "LXGW WenKai GB Screen"] {
            assert!(
                families.all.iter().any(|family| family == expected),
                "missing embedded font family {expected:?}; registered: {:?}",
                families.all
            );
        }
        assert!(families.serif.iter().any(|family| family == "Bitter"));
        assert!(families.sans_serif.iter().any(|family| family == "Roboto"));
        assert!(
            families
                .chinese
                .iter()
                .any(|family| family == "LXGW WenKai GB Screen")
        );
        assert!(!families.chinese.iter().any(|family| family == "Bitter"));
        assert!(!families.chinese.iter().any(|family| family == "Roboto"));
        #[cfg(target_os = "windows")]
        assert!(families.monospace.iter().any(|family| family == "Consolas"));
    }
}
