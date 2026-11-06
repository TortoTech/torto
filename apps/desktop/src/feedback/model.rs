pub(crate) use rebook_app_core::feedback::model::*;

#[derive(Clone)]
pub(crate) struct OpenContext {
    pub context: Context,
    // Used only by the preparation worker to determine PDF text source. Never uploaded.
    pub pdf_id: Option<String>,
}

impl OpenContext {
    pub fn shelf(ctx: &egui::Context) -> Self {
        let rect = ctx.content_rect();
        let scale = ctx.pixels_per_point();
        Self {
            context: Context {
                source: "shelf".into(),
                app: App {
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                os: Os {
                    name: std::env::consts::OS.into(),
                    version: "unknown".into(),
                    arch: std::env::consts::ARCH.into(),
                },
                display: Display {
                    width: (rect.width() * scale).clamp(1.0, 100_000.0) as u32,
                    height: (rect.height() * scale).clamp(1.0, 100_000.0) as u32,
                    scale: scale.clamp(0.1, 20.0),
                },
                book: None,
                location: None,
                reader: None,
            },
            pdf_id: None,
        }
    }
}
