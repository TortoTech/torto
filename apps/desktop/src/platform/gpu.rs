use std::sync::Arc;

use egui::TextureId;
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use kurbo::Affine;
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions as VelloOptions,
    Scene,
};
use wgpu::{TextureFormat, TextureUsages};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::app::DesktopApp;
use crate::reader::{ReaderFramePlan, ReaderPageTexture, ReaderScene};

struct PageTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    texture_id: TextureId,
    size: [u32; 2],
    logical_size: egui::Vec2,
    rendered_scene: Option<(u64, u64)>,
}

pub(super) struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    egui_renderer: Renderer,
    vello_renderer: VelloRenderer,
    page_target: Option<PageTarget>,
    retired_page_textures: Vec<TextureId>,
    clear_color: wgpu::Color,
    last_frame: Option<wgpu::Texture>,
    resize_pending: bool,
}

impl GpuState {
    pub(super) async fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .map_err(|error| error.to_string())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|error| error.to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("rebook-device"),
                ..Default::default()
            })
            .await
            .map_err(|error| error.to_string())?;
        let capabilities = surface.get_capabilities(&adapter);
        if crate::smoke::enabled() {
            crate::smoke::stage(&format!("gpu: {:?}", adapter.get_info()));
            device.on_uncaptured_error(Arc::new(|error| {
                crate::smoke::gpu_error(format!("{error}"))
            }));
        }
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(TextureFormat::is_srgb)
            .unwrap_or(capabilities.formats[0]);
        let mut surface_config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| "当前 GPU 不支持窗口 Surface".to_owned())?;
        surface_config.format = format;
        surface_config.usage = TextureUsages::RENDER_ATTACHMENT;
        if capabilities.usages.contains(TextureUsages::COPY_DST) {
            surface_config.usage |= TextureUsages::COPY_DST;
        }
        surface_config.view_formats = vec![format];
        surface_config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &surface_config);

        let egui_renderer = Renderer::new(&device, format, RendererOptions::default());
        let vello_renderer = VelloRenderer::new(
            &device,
            VelloOptions {
                antialiasing_support: AaSupport::area_only(),
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            surface,
            device,
            queue,
            surface_config,
            egui_renderer,
            vello_renderer,
            page_target: None,
            retired_page_textures: Vec::new(),
            last_frame: None,
            resize_pending: false,
            clear_color: wgpu::Color {
                r: 0.965,
                g: 0.957,
                b: 0.937,
                a: 1.0,
            },
        })
    }

    pub(super) fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0
            || size.height == 0
            || (self.surface_config.width == size.width
                && self.surface_config.height == size.height)
        {
            return;
        }
        self.resize_pending = true;
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    pub(super) fn set_clear_color(&mut self, color: wgpu::Color) {
        self.clear_color = color;
    }

    fn take_egui_input(
        &self,
        window: &Window,
        egui_state: &mut egui_winit::State,
    ) -> egui::RawInput {
        let mut input = egui_state.take_egui_input(window);
        input.max_texture_side =
            usize::try_from(self.device.limits().max_texture_dimension_2d).ok();
        input
    }

    fn acquire_surface_frame(
        &mut self,
        window: &Window,
    ) -> Result<Option<wgpu::SurfaceTexture>, String> {
        let mut recovery_attempted = false;
        loop {
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame) => return Ok(Some(frame)),
                wgpu::CurrentSurfaceTexture::Suboptimal(frame) if !recovery_attempted => {
                    crate::diagnostics::log(
                        "render.surface",
                        &[crate::diagnostics::Field::Text(
                            "status",
                            "suboptimal_recovering",
                        )],
                    );
                    // A suboptimal frame can still target the old DXGI swapchain
                    // extent after Windows has already resized the client area.
                    // Merely requesting another redraw is insufficient because
                    // `resize` sees the desired dimensions in `surface_config`
                    // and skips an otherwise-identical configure call forever.
                    drop(frame);
                    self.surface.configure(&self.device, &self.surface_config);
                    recovery_attempted = true;
                }
                wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                    crate::diagnostics::log(
                        "render.surface",
                        &[crate::diagnostics::Field::Text(
                            "status",
                            "suboptimal_deferred",
                        )],
                    );
                    window.request_redraw();
                    return Ok(Some(frame));
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
                    if !recovery_attempted =>
                {
                    crate::diagnostics::log(
                        "render.surface",
                        &[crate::diagnostics::Field::Text(
                            "status",
                            "lost_or_outdated_recovering",
                        )],
                    );
                    // A fullscreen/IME compositor transition can invalidate the
                    // swapchain between redraw events. Recover and acquire again
                    // now so the current redraw still presents a complete frame.
                    self.surface.configure(&self.device, &self.surface_config);
                    recovery_attempted = true;
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    crate::diagnostics::log(
                        "render.surface",
                        &[crate::diagnostics::Field::Text(
                            "status",
                            "lost_or_outdated_deferred",
                        )],
                    );
                    window.request_redraw();
                    return Ok(None);
                }
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    crate::diagnostics::log(
                        "render.surface",
                        &[crate::diagnostics::Field::Text(
                            "status",
                            "timeout_or_occluded",
                        )],
                    );
                    return Ok(None);
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    return Err("Surface validation failed".into());
                }
            }
        }
    }

    pub(super) fn render(
        &mut self,
        window: &Window,
        app: &mut DesktopApp,
        egui_ctx: &egui::Context,
        egui_state: &mut egui_winit::State,
    ) -> Result<(), String> {
        // Fullscreen transitions can deliver a redraw before their Resized event.
        // Always configure from the window's current client size before acquiring.
        self.resize(window.inner_size());

        if self.resize_pending {
            self.present_resize_preview(window)?;
        }

        let raw_input = self.take_egui_input(window, egui_state);
        let mut viewport_info = root_viewport_info(&raw_input);
        let mut plan = None;
        let mut output = egui_ctx.run_ui(raw_input, |ui| {
            plan = app.ui(ui, self.page_texture());
        });
        // ScaleFactorChanged is applied by begin_pass, so read this only after
        // run_ui instead of using the previous frame's scale.
        let pixels_per_point = egui_ctx.pixels_per_point();
        egui_state.handle_platform_output(window, std::mem::take(&mut output.platform_output));
        process_root_viewport_commands(window, egui_ctx, &mut viewport_info, &mut output);

        let mut page_target_recreated = false;
        if let Some(plan) = plan {
            let recreated = self.ensure_page_target(plan, pixels_per_point);
            page_target_recreated = recreated;
            let needs_render = recreated
                || self
                    .page_target
                    .as_ref()
                    .is_some_and(|target| reader_scene_needs_render(target.rendered_scene, plan));
            if needs_render && let Some(scene) = app.reader_scene() {
                self.render_reader_scene(&scene, plan, pixels_per_point)?;
            }
        }
        if page_target_recreated {
            // This frame intentionally retains the old texture at its old logical
            // size, so it is clipped instead of stretched. The next frame uses the
            // freshly rendered target without rerunning stateful UI in this frame.
            window.request_redraw();
        }

        let paint_jobs = egui_ctx.tessellate(output.shapes, pixels_per_point);
        let screen = ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point,
        };
        for (id, image_deltas) in output.textures_delta.set.drain() {
            for image_delta in image_deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, id, &image_delta);
            }
        }

        let Some(frame) = self.acquire_surface_frame(window)? else {
            self.free_egui_textures(&mut output.textures_delta);
            return Ok(());
        };
        // Retain the complete UI on the GPU, including panels and popups.
        let retain_frame = self.surface_config.usage.contains(TextureUsages::COPY_DST);
        if retain_frame {
            let size = frame.texture.size();
            if self
                .last_frame
                .as_ref()
                .is_none_or(|texture| texture.size() != size)
            {
                self.last_frame = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("retained-window-frame"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.surface_config.format,
                    usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
                    view_formats: &[],
                }));
            }
        }
        let target = if retain_frame {
            self.last_frame.as_ref().unwrap()
        } else {
            &frame.texture
        };
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("egui-encoder"),
            });
        let callback_commands = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.egui_renderer
                .render(&mut pass.forget_lifetime(), &paint_jobs, &screen);
        }
        if retain_frame {
            encoder.copy_texture_to_texture(
                self.last_frame.as_ref().unwrap().as_image_copy(),
                frame.texture.as_image_copy(),
                frame.texture.size(),
            );
        }
        self.queue
            .submit(callback_commands.into_iter().chain([encoder.finish()]));
        window.pre_present_notify();
        frame.present();
        if crate::smoke::enabled() {
            self.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_secs(10)),
                })
                .map_err(|error| error.to_string())?;
            let reader_ready = plan.is_some()
                && !page_target_recreated
                && self
                    .page_target
                    .as_ref()
                    .is_some_and(|target| target.rendered_scene.is_some());
            crate::smoke::presented(reader_ready, app.startup_error())?;
        }
        for id in self.retired_page_textures.drain(..) {
            self.egui_renderer.free_texture(&id);
        }
        self.free_egui_textures(&mut output.textures_delta);
        Ok(())
    }

    /// Present old pixels without scaling before expensive UI layout starts.
    fn present_resize_preview(&mut self, window: &Window) -> Result<(), String> {
        let Some(frame) = self.acquire_surface_frame(window)? else {
            return Ok(());
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("resize-preview"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("resize-background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        if let Some(previous) = &self.last_frame {
            encoder.copy_texture_to_texture(
                previous.as_image_copy(),
                frame.texture.as_image_copy(),
                retained_frame_extent(previous.size(), frame.texture.size()),
            );
        }
        self.queue.submit([encoder.finish()]);
        window.pre_present_notify();
        frame.present();
        self.resize_pending = false;
        // Keep the last complete frame, not this temporary partially empty one.
        Ok(())
    }

    fn free_egui_textures(&mut self, textures_delta: &mut egui::TexturesDelta) {
        for id in textures_delta.free.drain() {
            self.egui_renderer.free_texture(&id);
        }
    }

    fn page_texture(&self) -> Option<ReaderPageTexture> {
        self.page_target.as_ref().map(|target| ReaderPageTexture {
            id: target.texture_id,
            size: target.logical_size,
        })
    }

    fn ensure_page_target(&mut self, plan: ReaderFramePlan, pixels_per_point: f32) -> bool {
        let size = [
            physical_dimension(plan.rect.width(), pixels_per_point),
            physical_dimension(plan.rect.height(), pixels_per_point),
        ];
        if self
            .page_target
            .as_ref()
            .is_some_and(|target| target.size == size)
        {
            return false;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("reader-vello-target"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Register a new native texture instead of rebinding the texture ID used by
        // the egui shapes already built for this frame. The previous ID stays alive
        // until those shapes have been submitted, so rapid panel motion can never
        // sample a half-swapped Vello target.
        let texture_id = self.egui_renderer.register_native_texture(
            &self.device,
            &view,
            wgpu::FilterMode::Linear,
        );
        let previous = self.page_target.replace(PageTarget {
            _texture: texture,
            view,
            texture_id,
            size,
            logical_size: plan.rect.size(),
            rendered_scene: None,
        });
        if let Some(previous) = previous {
            self.retired_page_textures.push(previous.texture_id);
        }
        true
    }

    fn render_reader_scene(
        &mut self,
        scene: &ReaderScene,
        plan: ReaderFramePlan,
        pixels_per_point: f32,
    ) -> Result<(), String> {
        // Vello 0.10 can still omit a previously resolved ImageData on subsequent
        // render_to_texture calls (linebender/vello#1809). Explicitly marking
        // every image referenced by this scene dirty makes the persistent atlas
        // re-upload those pixels before the scene is replayed.
        if scene.refresh_image_atlas {
            #[cfg(debug_assertions)]
            if !scene.images.is_empty() && {
                // Keep the atlas workaround, but avoid opening a log file for
                // every animation frame containing an image.
                static SAMPLES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                SAMPLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 60 == 0
            } {
                crate::diagnostics::log(
                    "render.reader_images",
                    &[
                        crate::diagnostics::Field::Text("action", "refresh_atlas"),
                        crate::diagnostics::Field::Usize("count", scene.images.len()),
                        crate::diagnostics::Field::Bool("sampled", true),
                    ],
                );
            }
            for image in scene.images.iter() {
                self.vello_renderer.mark_override_image_dirty(image);
            }
        }
        let Some(target) = self.page_target.as_mut() else {
            return Ok(());
        };
        let mut physical_scene = Scene::new();
        physical_scene.append(
            &scene.scene,
            Some(Affine::scale(f64::from(pixels_per_point))),
        );
        self.vello_renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                &physical_scene,
                &target.view,
                &RenderParams {
                    base_color: plan.background,
                    width: target.size[0],
                    height: target.size[1],
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|error| error.to_string())?;
        target.rendered_scene = Some((plan.scene_id, plan.scene_revision));
        Ok(())
    }
}

fn root_viewport_info(input: &egui::RawInput) -> egui::ViewportInfo {
    input
        .viewports
        .get(&egui::ViewportId::ROOT)
        .cloned()
        .unwrap_or_default()
}

fn process_root_viewport_commands(
    window: &Window,
    egui_ctx: &egui::Context,
    viewport_info: &mut egui::ViewportInfo,
    output: &mut egui::FullOutput,
) {
    let Some(root_output) = output.viewport_output.get_mut(&egui::ViewportId::ROOT) else {
        return;
    };
    let mut actions_requested = Vec::new();
    egui_winit::process_viewport_commands(
        egui_ctx,
        viewport_info,
        root_output.commands.drain(..),
        window,
        &mut actions_requested,
    );
    if !actions_requested.is_empty() {
        crate::diagnostics::log(
            "viewport.actions.unsupported",
            &[crate::diagnostics::Field::Usize(
                "count",
                actions_requested.len(),
            )],
        );
    }
}

fn reader_scene_needs_render(rendered_scene: Option<(u64, u64)>, plan: ReaderFramePlan) -> bool {
    rendered_scene != Some((plan.scene_id, plan.scene_revision))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn physical_dimension(points: f32, pixels_per_point: f32) -> u32 {
    let pixels = (points * pixels_per_point).ceil();
    if !pixels.is_finite() || pixels <= 1.0 {
        return 1;
    }
    pixels.min(u32::MAX as f32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(scene_id: u64, scene_revision: u64) -> ReaderFramePlan {
        ReaderFramePlan {
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(100.0)),
            scene_id,
            scene_revision,
            background: peniko::Color::BLACK,
        }
    }

    #[test]
    fn a_new_reader_renders_even_when_its_revision_matches_the_previous_reader() {
        assert!(reader_scene_needs_render(Some((7, 1)), plan(8, 1)));
        assert!(!reader_scene_needs_render(Some((8, 1)), plan(8, 1)));
        assert!(reader_scene_needs_render(Some((8, 1)), plan(8, 2)));
    }
}

// Copy the intersection at origin (0, 0); never scale the previous frame.
fn retained_frame_extent(previous: wgpu::Extent3d, current: wgpu::Extent3d) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: previous.width.min(current.width),
        height: previous.height.min(current.height),
        depth_or_array_layers: 1,
    }
}

#[cfg(test)]
mod resize_preview_tests {
    use super::retained_frame_extent;
    #[test]
    fn retained_pixels_are_clipped_not_stretched() {
        let size = |width, height| wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        assert_eq!(
            retained_frame_extent(size(800, 600), size(1200, 900)),
            size(800, 600)
        );
        assert_eq!(
            retained_frame_extent(size(1200, 900), size(800, 600)),
            size(800, 600)
        );
        assert_eq!(
            retained_frame_extent(size(800, 900), size(1200, 600)),
            size(800, 600)
        );
    }
}
