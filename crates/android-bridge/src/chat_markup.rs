use crate::*;
#[uniffi::export]
pub async fn chat_markdown(source: String) -> Result<String> {
    work(move || {
        if source.len() > 512 * 1024 {
            return Err(error("对话内容超过显示限制"));
        }
        serde_json::to_string(&rebook_app_core::chat_markdown::parse(&source)).map_err(error)
    })
    .await
}
#[uniffi::export]
pub async fn render_chat_formula(
    latex: String,
    font_size: f32,
    foreground: u32,
) -> Result<Arc<MobilePage>> {
    work(move || {
        interactions::formula_page(
            &latex,
            font_size,
            rebook_publication::Rgba {
                red: (foreground >> 16) as u8,
                green: (foreground >> 8) as u8,
                blue: foreground as u8,
                alpha: 255,
            },
            1024,
        )
    })
    .await
}
