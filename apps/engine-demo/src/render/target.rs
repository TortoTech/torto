use std::sync::mpsc;
use std::time::Instant;

use peniko::Color;
use rebook_reader::{ReaderPosition, ReaderSpread};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions as VelloOptions,
};
use wgpu::{
    BufferAsyncError, BufferDescriptor, BufferUsages, CommandEncoderDescriptor, Device,
    DeviceDescriptor, Extent3d, Instance, MapMode, PollType, PowerPreference, Queue,
    RequestAdapterOptions, Texture, TextureAspect, TextureDescriptor, TextureDimension,
    TextureFormat, TextureUsages, TextureViewDescriptor,
};

use super::compositor::ReaderCompositor;
use super::metrics::RenderMetrics;
use super::scene::{OverlaySet, PageSceneKey, SpreadSceneKey};

use super::scene_cache::{DEFAULT_SCENE_CACHE_CAPACITY, SpreadSceneCache};

pub struct OffscreenTarget {
    device: Device,
    queue: Queue,
    vello_renderer: VelloRenderer,
    scene_cache: SpreadSceneCache,
}

impl OffscreenTarget {
    pub async fn new() -> Result<Self, String> {
        let instance = Instance::default();
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| format!("Failed to request GPU adapter: {e}"))?;

        let (device, queue) = adapter
            .request_device(&DeviceDescriptor {
                label: Some("rebook-offscreen-device"),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("Failed to request GPU device: {e}"))?;

        let vello_renderer = VelloRenderer::new(
            &device,
            VelloOptions {
                antialiasing_support: AaSupport::area_only(),
                ..Default::default()
            },
        )
        .map_err(|e| format!("Failed to create Vello renderer: {e}"))?;

        Ok(Self {
            device,
            queue,
            vello_renderer,
            scene_cache: SpreadSceneCache::new(DEFAULT_SCENE_CACHE_CAPACITY),
        })
    }

    pub async fn with_profile(
        profile: super::scene_cache::ResourceProfile,
    ) -> Result<Self, String> {
        let mut target = Self::new().await?;
        target.scene_cache = SpreadSceneCache::with_profile(profile);
        Ok(target)
    }

    pub fn render_spread_to_png(
        &mut self,
        spread: &ReaderSpread,
        width: u32,
        height: u32,
    ) -> Result<(Vec<u8>, RenderMetrics), String> {
        validate_dimensions(width, height)?;

        let mut metrics = RenderMetrics {
            width,
            height,
            ..Default::default()
        };

        // 1. Build Scene via ReaderCompositor & Cache
        let scene_start = Instant::now();
        let key = SpreadSceneKey {
            primary: PageSceneKey {
                position: ReaderPosition {
                    section_index: 0,
                    segment_index: 0,
                    page_index: 0,
                },
                layout_generation: 0,
            },
            secondary: None,
            width,
            height,
        };
        let layers = self.scene_cache.get_or_build(&key, spread);
        let overlays = OverlaySet::default();
        let scene = ReaderCompositor::compose_spread_scene(&layers, spread, &overlays, None);
        metrics.scene_build = scene_start.elapsed();
        metrics.cache = self.scene_cache.metrics().clone();

        // 2. Mark images dirty if present
        let mut image_count = 0;
        for image in layers.images.iter() {
            self.vello_renderer.mark_override_image_dirty(image);
            image_count += 1;
        }
        metrics.image_count = image_count;

        // 3. Render Target Texture
        let texture = self.device.create_texture(&TextureDescriptor {
            label: Some("offscreen-render-target"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&TextureViewDescriptor::default());

        // 4. Render to texture
        let submit_start = Instant::now();
        self.vello_renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                &scene,
                &view,
                &RenderParams {
                    base_color: Color::WHITE,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| format!("Vello render_to_texture failed: {e}"))?;
        metrics.gpu_submit = submit_start.elapsed();

        // 5. Readback texture with row padding removed
        let readback_start = Instant::now();
        let raw_rgba = self.readback_texture(&texture, width, height)?;
        metrics.readback = readback_start.elapsed();

        // 6. Encode PNG
        let encode_start = Instant::now();
        let png_bytes = encode_png(&raw_rgba, width, height)?;
        metrics.png_encode = encode_start.elapsed();

        Ok((png_bytes, metrics))
    }

    fn readback_texture(
        &self,
        texture: &Texture,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
        let buffer_size = u64::from(padded_bytes_per_row) * u64::from(height);

        let staging_buffer = self.device.create_buffer(&BufferDescriptor {
            label: Some("staging-readback-buffer"),
            size: buffer_size,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("readback-encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(Some(encoder.finish()));

        let buffer_slice = staging_buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        buffer_slice.map_async(MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| format!("Device poll failed: {e:?}"))?;

        receiver
            .recv()
            .map_err(|e| format!("Receiver error: {e:?}"))?
            .map_err(|e: BufferAsyncError| format!("Failed to map buffer: {e:?}"))?;

        let mapped = buffer_slice.get_mapped_range();
        let unpadded = remove_row_padding(&mapped, width, height, padded_bytes_per_row);
        drop(mapped);
        staging_buffer.unmap();

        Ok(unpadded)
    }
}

pub fn remove_row_padding(
    padded_data: &[u8],
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
) -> Vec<u8> {
    let unpadded_bytes_per_row = (width * 4) as usize;
    let padded_stride = padded_bytes_per_row as usize;
    let mut result = Vec::with_capacity(unpadded_bytes_per_row * height as usize);

    for row in 0..height as usize {
        let start = row * padded_stride;
        let end = start + unpadded_bytes_per_row;
        if end <= padded_data.len() {
            result.extend_from_slice(&padded_data[start..end]);
        }
    }

    result
}

pub fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("Dimensions must be non-zero".to_string());
    }
    // Limit to reasonable texture dimension to prevent huge memory allocations
    if width > 16384 || height > 16384 {
        return Err(format!(
            "Dimensions {width}x{height} exceed maximum allowed 16384"
        ));
    }
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("Dimensions {width}x{height} caused byte overflow"))?;
    Ok(())
}

fn encode_png(raw_rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut png_bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
    image::ImageEncoder::write_image(
        encoder,
        raw_rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok(png_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_row_padding() {
        let width = 2;
        let height = 2;
        let padded_bytes_per_row = 256; // padded to 256 bytes

        let mut padded = vec![0u8; (padded_bytes_per_row * height) as usize];
        // Row 0 data
        padded[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        // Row 1 data
        padded[256..264].copy_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16]);

        let result = remove_row_padding(&padded, width, height, padded_bytes_per_row);
        assert_eq!(
            result,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn test_validate_dimensions() {
        assert!(validate_dimensions(0, 100).is_err());
        assert!(validate_dimensions(100, 0).is_err());
        assert!(validate_dimensions(800, 1000).is_ok());
        assert!(validate_dimensions(20000, 20000).is_err());
        assert!(validate_dimensions(u32::MAX, u32::MAX).is_err());
    }
}
