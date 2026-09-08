use std::sync::Arc;

use peniko::Color;
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions as VelloOptions,
    Scene,
};
use wgpu::{
    CurrentSurfaceTexture, Device, DeviceDescriptor, Instance, PowerPreference, Queue,
    RequestAdapterOptions, Surface, SurfaceConfiguration, TextureFormat, TextureUsages,
};
use winit::dpi::PhysicalSize;
use winit::window::Window;

pub struct SurfaceRenderer {
    _window: Arc<Window>,
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    surface_config: SurfaceConfiguration,
    vello_renderer: VelloRenderer,
    clear_color: Color,
}

impl SurfaceRenderer {
    pub async fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let instance = Instance::default();
        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|e| format!("Failed to create wgpu surface: {e}"))?;

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| format!("Failed to request GPU adapter: {e}"))?;

        let (device, queue) = adapter
            .request_device(&DeviceDescriptor {
                label: Some("rebook-window-device"),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("Failed to request GPU device: {e}"))?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(TextureFormat::is_srgb)
            .unwrap_or(capabilities.formats[0]);

        let mut surface_config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| "Surface is not supported by GPU adapter".to_string())?;
        surface_config.format = format;
        surface_config.usage = TextureUsages::RENDER_ATTACHMENT;
        surface_config.view_formats = vec![format];
        surface_config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &surface_config);

        let vello_renderer = VelloRenderer::new(
            &device,
            VelloOptions {
                antialiasing_support: AaSupport::area_only(),
                ..Default::default()
            },
        )
        .map_err(|e| format!("Failed to create Vello renderer: {e}"))?;

        Ok(Self {
            _window: window,
            surface,
            device,
            queue,
            surface_config,
            vello_renderer,
            clear_color: Color::WHITE,
        })
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        if self.surface_config.width == size.width && self.surface_config.height == size.height {
            return;
        }
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    pub fn mark_image_dirty(&mut self, image: &peniko::ImageData) {
        self.vello_renderer.mark_override_image_dirty(image);
    }

    pub fn render_frame(&mut self, scene: &Scene) -> Result<(), String> {
        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) => frame,
            CurrentSurfaceTexture::Suboptimal(frame) => {
                self.surface.configure(&self.device, &self.surface_config);
                frame
            }
            CurrentSurfaceTexture::Lost | CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.surface_config);
                return Ok(());
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            CurrentSurfaceTexture::Validation => {
                return Err("Surface validation failed".to_string());
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        self.vello_renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                scene,
                &view,
                &RenderParams {
                    base_color: self.clear_color,
                    width: self.surface_config.width,
                    height: self.surface_config.height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| format!("Vello render_to_texture failed: {e}"))?;

        frame.present();
        Ok(())
    }
}
