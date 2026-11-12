//! Shared, punctuation-delimited source/note labels for adjacent media.
pub fn is_source(text: &str) -> bool {
    let text = text.trim_start();
    text.strip_prefix('©').is_some_and(|tail| !tail.trim().is_empty())
        || text.to_lowercase().starts_with("copyright ©")
        || prefix(
        text,
        &[
            "source",
            "sources",
            "credit",
            "credits",
            "来源",
            "來源",
            "图片来源",
            "圖片來源",
            "图源",
            "圖源",
            "资料来源",
            "資料來源",
            "数据来源",
            "數據來源",
            "版权",
            "版權",
        ],
    )
}
pub fn is_annotation(text: &str) -> bool {
    is_source(text)
        || prefix(
            text,
            &[
                "note",
                "notes",
                "abbreviations",
                "注",
                "注释",
                "註",
                "註釋",
                "缩写",
                "縮寫",
                "数据说明",
                "數據說明",
                "资料说明",
                "資料說明",
            ],
        )
}
fn prefix(text: &str, prefixes: &[&str]) -> bool {
    let lower = text.trim_start().to_lowercase();
    prefixes.iter().any(|p| {
        lower
            .strip_prefix(p)
            .is_some_and(|tail| tail.trim_start().starts_with([':', '：']))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_labels_require_a_delimiter() {
        for text in [
            "数据来源：CEIC",
            "资料来源: Authors",
            " Sources : Authors",
            "圖片來源：A",
            "© Daniel Willingham",
            "Copyright © Diagram author",
            "版权：作者",
        ] {
            assert!(is_source(text), "{text}");
        }
        for text in ["数据说明：说明", "Notes: Explanation", "缩写：ABC"] {
            assert!(is_annotation(text), "{text}");
        }
        for text in [
            "Source material is discussed below",
            "数据来源的选择很重要",
            "注释是正文的一部分",
            "Copyright law protects illustrations.",
            "©",
        ] {
            assert!(!is_annotation(text), "{text}");
        }
    }
}
