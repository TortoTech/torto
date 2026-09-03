use std::sync::Arc;

use peniko::Blob;

const LITERATA_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf");
const LITERATA_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/Literata-Italic-opsz-wght.ttf");
pub(crate) fn cjk_font_bytes() -> &'static [u8] {
    rebook_formats::cjk_fallback_font_bytes()
}

pub fn embedded_reader_fonts() -> Arc<[Blob<u8>]> {
    [
        font_blob(LITERATA_REGULAR),
        font_blob(LITERATA_ITALIC),
        font_blob(cjk_font_bytes()),
    ]
    .into()
}

fn font_blob(bytes: &'static [u8]) -> Blob<u8> {
    Blob::new(Arc::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::LayoutEngine;

    #[test]
    fn embedded_font_assets_are_non_empty() {
        let fonts = embedded_reader_fonts();
        assert_eq!(fonts.len(), 3);
        assert!(fonts.iter().all(|font| !font.is_empty()));
    }

    #[test]
    fn embedded_fonts_register_with_reader_family_names() {
        let fonts = embedded_reader_fonts();
        let mut engine = LayoutEngine::with_fonts(fonts.iter().cloned());
        #[cfg(target_os = "windows")]
        let discovered = engine.available_font_families();
        let mut families = engine.available_reader_font_families();
        families.include_configured(&rebook_layout::ReaderTypography::default());

        for expected in ["Literata", "LXGW WenKai GB Screen"] {
            assert!(
                families.all.iter().any(|family| family == expected),
                "missing embedded font family {expected:?}; registered: {:?}",
                families.all
            );
        }
        assert!(families.serif.iter().any(|family| family == "Literata"));
        assert!(
            families
                .chinese
                .iter()
                .any(|family| family == "LXGW WenKai GB Screen")
        );
        #[cfg(target_os = "windows")]
        {
            assert!(families.monospace.iter().any(|family| family == "Consolas"));
            for simsun_alias in ["SimSun", "宋体", "NSimSun", "新宋体"] {
                if discovered.iter().any(|family| family == simsun_alias) {
                    assert!(
                        !families.all.iter().any(|family| family == simsun_alias),
                        "Vello-incompatible embedded-bitmap font leaked into reader options"
                    );
                }
            }
        }
    }
}
