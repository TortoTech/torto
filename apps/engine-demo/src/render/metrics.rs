use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct RenderMetrics {
    pub scene_build: Duration,
    pub gpu_submit: Duration,
    pub readback: Duration,
    pub png_encode: Duration,
    pub width: u32,
    pub height: u32,
    pub image_count: usize,
    pub cache: super::scene_cache::CacheMetrics,
}
