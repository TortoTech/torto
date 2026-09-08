use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kurbo::Affine;
use rebook_engine::{
    Engine, EngineBook, EngineReader, NavigationPreparation, PageDirection, PreparedNavigation,
    ReaderConfig,
};
use rebook_layout::{LayoutViewport, ReaderStyle};
use vello::Scene;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::metrics::{FrameStats, MetricsFormat, PipelineMetrics, RollingWindowMetrics};
use crate::render::scene::{OverlaySet, PageSceneKey, SpreadSceneKey};
use crate::render::scene_cache::{MemoryPressure, ResourceProfile};
use crate::render::{ReaderCompositor, SpreadSceneCache};
use crate::surface::SurfaceRenderer;
use crate::transition::{
    DragGesture, TransitionKind, TransitionPolicy, TransitionState, evaluate_settle_progress,
    slide_transforms,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyState {
    Clean,
    Scene,
    Surface,
    Animation,
}

pub struct DemoApplication {
    book_path: PathBuf,
    metrics_format: Option<MetricsFormat>,
    metrics_file: Option<PathBuf>,
    profile: ResourceProfile,
    engine: Engine,
    book: Option<EngineBook>,
    reader: Option<EngineReader>,
    gpu: Option<SurfaceRenderer>,
    scene_cache: SpreadSceneCache,
    window: Option<Arc<Window>>,
    viewport: PhysicalSize<u32>,
    dirty: DirtyState,
    layout_generation: u64,
    show_metrics: bool,
    rolling_metrics: RollingWindowMetrics,
    pipeline_metrics: Option<PipelineMetrics>,
    last_frame_instant: Option<Instant>,
    // Transition & Interaction state
    transition_kind: TransitionKind,
    transition_state: TransitionState,
    transition_policy: TransitionPolicy,
    active_pointer_id: Option<u64>,
    cursor_position: Option<PhysicalPosition<f64>>,
}

impl DemoApplication {
    pub fn new(
        book_path: PathBuf,
        metrics_format: Option<MetricsFormat>,
        metrics_file: Option<PathBuf>,
        profile: ResourceProfile,
        transition: Option<TransitionKind>,
    ) -> Self {
        Self {
            book_path,
            metrics_format,
            metrics_file,
            profile,
            engine: Engine::default(),
            book: None,
            reader: None,
            gpu: None,
            scene_cache: SpreadSceneCache::with_profile(profile),
            window: None,
            viewport: PhysicalSize::new(800, 1000),
            dirty: DirtyState::Scene,
            layout_generation: 0,
            show_metrics: false,
            rolling_metrics: RollingWindowMetrics::new(120),
            pipeline_metrics: None,
            last_frame_instant: None,
            transition_kind: transition.unwrap_or(TransitionKind::Slide),
            transition_state: TransitionState::Idle,
            transition_policy: TransitionPolicy::default(),
            active_pointer_id: None,
            cursor_position: None,
        }
    }

    fn ensure_reader(&mut self, width: u32, height: u32) -> Result<(), String> {
        let mut file_read_dur = Duration::ZERO;
        let mut open_dur = Duration::ZERO;

        if self.book.is_none() {
            let start_read = Instant::now();
            let bytes = fs::read(&self.book_path)
                .map_err(|e| format!("Failed to read {}: {e}", self.book_path.display()))?;
            file_read_dur = start_read.elapsed();

            let file_name = self
                .book_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("book");

            let start_open = Instant::now();
            let book = self
                .engine
                .open_bytes(bytes, file_name)
                .map_err(|e| format!("Failed to open book: {e}"))?;
            open_dur = start_open.elapsed();
            self.book = Some(book);
        }

        let book = self.book.as_ref().unwrap();
        if self.reader.is_none() {
            let start_reader = Instant::now();
            let reader = self
                .engine
                .create_reader(
                    book,
                    ReaderConfig {
                        viewport: LayoutViewport { width, height },
                        style: ReaderStyle::default(),
                        locator: None,
                    },
                )
                .map_err(|e| format!("Failed to create reader: {e}"))?;
            let reader_dur = start_reader.elapsed();
            self.reader = Some(reader);
            self.layout_generation += 1;

            self.pipeline_metrics = Some(PipelineMetrics::from_durations(
                file_read_dur,
                open_dur,
                reader_dur,
                Duration::ZERO,
                Duration::ZERO,
                None,
            ));
        }

        Ok(())
    }

    fn trigger_keyboard_turn(&mut self, direction: PageDirection) {
        if self.transition_kind == TransitionKind::None {
            if let Some(reader) = self.reader.as_mut() {
                let _ = reader.try_turn_page(direction);
                self.dirty = DirtyState::Scene;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }

        if !self.transition_state.is_idle() {
            return;
        }

        let Some(reader) = self.reader.as_mut() else {
            return;
        };

        match reader.prepare_navigation(direction) {
            Ok(NavigationPreparation::Ready(prepared)) => {
                self.transition_state = TransitionState::Settling {
                    prepared,
                    from: 0.0,
                    to: 1.0,
                    started_at: Instant::now(),
                    duration: self.transition_policy.default_duration,
                };
                self.dirty = DirtyState::Animation;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Ok(NavigationPreparation::Pending(token)) => {
                self.transition_state = TransitionState::Preparing {
                    direction,
                    token,
                    requested_at: Instant::now(),
                };
                self.dirty = DirtyState::Animation;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Ok(NavigationPreparation::Boundary) => {
                // At boundary, do nothing
            }
            Err(e) => {
                eprintln!("Navigation preparation error: {e}");
            }
        }
    }

    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    fn render_frame(&mut self) -> Result<(), String> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(());
        };
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };

        // Poll pending navigation if in Preparing state
        if let TransitionState::Preparing {
            token,
            direction: _,
            requested_at,
        } = self.transition_state
        {
            match reader.poll_navigation(token) {
                Ok(NavigationPreparation::Ready(prepared)) => {
                    self.transition_state = TransitionState::Settling {
                        prepared,
                        from: 0.0,
                        to: 1.0,
                        started_at: Instant::now(),
                        duration: self.transition_policy.default_duration,
                    };
                }
                Ok(NavigationPreparation::Pending(_)) => {
                    // Still preparing
                    if requested_at.elapsed() > Duration::from_millis(500) {
                        // Timeout safety: cancel and return to idle
                        reader.cancel_navigation(token);
                        self.transition_state = TransitionState::Idle;
                        self.dirty = DirtyState::Scene;
                    } else if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                Ok(NavigationPreparation::Boundary) | Err(_) => {
                    self.transition_state = TransitionState::Idle;
                    self.dirty = DirtyState::Scene;
                }
            }
        }

        // Check settling animation progress
        let mut commit_destination: Option<PreparedNavigation> = None;
        let mut cancel_token: Option<rebook_engine::NavigationToken> = None;
        let mut current_progress: Option<(PageDirection, f32)> = None;

        match &self.transition_state {
            TransitionState::Settling {
                prepared,
                from,
                to,
                started_at,
                duration,
            } => {
                let (p, done) =
                    evaluate_settle_progress(*from, *to, started_at.elapsed(), *duration);
                current_progress = Some((prepared.direction(), p));
                if done {
                    if (*to - 1.0).abs() < 1e-4 {
                        // Reached 1.0 -> commit
                        if let TransitionState::Settling { prepared, .. } =
                            std::mem::replace(&mut self.transition_state, TransitionState::Idle)
                        {
                            commit_destination = Some(prepared);
                        }
                    } else {
                        // Reached 0.0 -> cancel
                        let token = prepared.token();
                        self.transition_state = TransitionState::Idle;
                        cancel_token = Some(token);
                    }
                } else if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            TransitionState::Interactive {
                prepared, progress, ..
            } => {
                current_progress = Some((prepared.direction(), *progress));
            }
            _ => {}
        }

        // Commit or cancel completed settling before or after rendering
        if let Some(prepared) = commit_destination {
            if let Err(e) = reader.commit_navigation(prepared) {
                eprintln!("Failed to commit navigation: {e}");
            }
            self.scene_cache.clear_pins();
            self.dirty = DirtyState::Scene;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        } else if let Some(token) = cancel_token {
            reader.cancel_navigation(token);
            self.scene_cache.clear_pins();
            self.dirty = DirtyState::Scene;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }

        let frame_start = Instant::now();
        let frame_interval = self
            .last_frame_instant
            .map_or(Duration::from_millis(16), |prev| prev.elapsed());
        self.last_frame_instant = Some(frame_start);

        let source_spread = reader
            .current_spread()
            .map_err(|e| format!("Failed to get spread: {e}"))?;

        let source_pos = reader.current_position();
        let source_key = SpreadSceneKey {
            primary: PageSceneKey {
                position: source_pos,
                layout_generation: self.layout_generation,
            },
            secondary: None,
            width: self.viewport.width,
            height: self.viewport.height,
        };

        let pre_builds = self.scene_cache.metrics().builds;

        let final_scene = if let Some((direction, progress)) = current_progress {
            // In transition: compose source and destination spreads
            let destination_spread = match &self.transition_state {
                TransitionState::Settling { prepared, .. }
                | TransitionState::Interactive { prepared, .. } => {
                    prepared.destination_spread().clone()
                }
                _ => unreachable!(),
            };
            let destination_pos = match &self.transition_state {
                TransitionState::Settling { prepared, .. }
                | TransitionState::Interactive { prepared, .. } => prepared.destination(),
                _ => unreachable!(),
            };

            let dest_key = SpreadSceneKey {
                primary: PageSceneKey {
                    position: destination_pos,
                    layout_generation: self.layout_generation,
                },
                secondary: None,
                width: self.viewport.width,
                height: self.viewport.height,
            };

            // Pin both source and destination keys so neither is evicted
            self.scene_cache.clear_pins();
            self.scene_cache.pin(source_key.clone());
            self.scene_cache.pin(dest_key.clone());

            let source_layers = self.scene_cache.get_or_build(&source_key, &source_spread);
            let dest_layers = self
                .scene_cache
                .get_or_build(&dest_key, &destination_spread);

            for image in source_layers.images.iter().chain(dest_layers.images.iter()) {
                gpu.mark_image_dirty(image);
            }

            let overlays = OverlaySet::default();
            let (source_x, dest_x) =
                slide_transforms(direction, progress, self.viewport.width as f32);

            let mut scene = Scene::new();
            let source_scene = ReaderCompositor::compose_spread_scene(
                &source_layers,
                &source_spread,
                &overlays,
                Some(Affine::translate((f64::from(source_x), 0.0))),
            );
            let dest_scene = ReaderCompositor::compose_spread_scene(
                &dest_layers,
                &destination_spread,
                &overlays,
                Some(Affine::translate((f64::from(dest_x), 0.0))),
            );
            scene.append(&source_scene, None);
            scene.append(&dest_scene, None);
            scene
        } else {
            // Idle or TrackingSlop: compose single current spread
            self.scene_cache.clear_pins();
            self.scene_cache.pin(source_key.clone());

            let layers = self.scene_cache.get_or_build(&source_key, &source_spread);
            for image in layers.images.iter() {
                gpu.mark_image_dirty(image);
            }
            let overlays = OverlaySet::default();
            ReaderCompositor::compose_spread_scene(&layers, &source_spread, &overlays, None)
        };

        let scene_builds = self.scene_cache.metrics().builds - pre_builds;
        gpu.render_frame(&final_scene)?;

        let cpu_duration = frame_start.elapsed();
        let missed_60hz = cpu_duration > Duration::from_millis(16);
        let missed_120hz = cpu_duration > Duration::from_millis(8);

        let stats = FrameStats {
            interval_ms: frame_interval.as_secs_f64() * 1000.0,
            cpu_duration_ms: cpu_duration.as_secs_f64() * 1000.0,
            gpu_duration_ms: None,
            scene_builds,
            target_recreations: 0,
            missed_60hz,
            missed_120hz,
        };
        self.rolling_metrics.record_frame(&stats);

        if self.show_metrics
            && let Some(window) = &self.window
        {
            let snapshot = reader.snapshot();
            let (p50, p95, _p99) = self.rolling_metrics.cpu_percentiles();
            let cache_metrics = self.scene_cache.metrics();
            window.set_title(&format!(
                "Torto Demo - Page {}/{} | [{}] | CPU p50: {:.2?} p95: {:.2?} | Cache: hits={}, misses={}, builds={}",
                snapshot.location.page_index + 1,
                snapshot.location.page_count,
                self.transition_kind.as_str(),
                p50,
                p95,
                cache_metrics.hits,
                cache_metrics.misses,
                cache_metrics.builds
            ));
        }

        if self.transition_state.is_idle() {
            self.dirty = DirtyState::Clean;
        }

        Ok(())
    }

    fn dump_metrics_if_requested(&self) {
        if self.metrics_format.is_none() && self.metrics_file.is_none() {
            return;
        }

        let ctx = crate::metrics::ReportContext {
            viewport: (self.viewport.width, self.viewport.height),
            adapter_backend: "wgpu-vello-surface".to_string(),
            spread_mode: "Single".to_string(),
            transition_kind: self.transition_kind.as_str().to_string(),
            resource_profile: format!("{:?}", self.profile),
        };
        let report = self.rolling_metrics.build_report(
            ctx,
            self.pipeline_metrics.clone(),
            self.scene_cache.metrics().clone(),
        );

        let formatted = match self.metrics_format {
            Some(MetricsFormat::Json) => serde_json::to_string_pretty(&report).unwrap_or_default(),
            Some(MetricsFormat::Text) => format!("{report:#?}"),
            None => String::new(),
        };

        if let Some(format) = self.metrics_format {
            println!("\n=== Final Window Metrics ({format:?}) ===");
            println!("{formatted}");
        }

        if let Some(path) = &self.metrics_file {
            let content = if formatted.is_empty() {
                serde_json::to_string_pretty(&report).unwrap_or_default()
            } else {
                formatted
            };
            let _ = fs::write(path, content);
        }
    }

    fn handle_pointer_down(&mut self, pointer_id: u64, x: f32) {
        if self.transition_kind == TransitionKind::None {
            return;
        }
        // In v1, ignore secondary pointers
        if self.active_pointer_id.is_some() {
            return;
        }

        if !self.transition_state.is_idle() {
            return;
        }

        self.active_pointer_id = Some(pointer_id);
        self.transition_state = TransitionState::TrackingSlop {
            gesture: DragGesture::new(pointer_id, x),
        };
    }

    #[allow(clippy::cast_precision_loss)]
    fn handle_pointer_move(&mut self, pointer_id: u64, x: f32) {
        if self.transition_kind == TransitionKind::None {
            return;
        }
        if self.active_pointer_id != Some(pointer_id) {
            return;
        }

        let width = self.viewport.width as f32;
        if width <= 0.0 {
            return;
        }

        match &mut self.transition_state {
            TransitionState::TrackingSlop { gesture } => {
                gesture.record_move(x);
                let delta = gesture.horizontal_delta();
                if delta.abs() >= self.transition_policy.slop_threshold {
                    let intended_direction = if delta < 0.0 {
                        PageDirection::Next
                    } else {
                        PageDirection::Previous
                    };

                    let Some(reader) = self.reader.as_mut() else {
                        return;
                    };
                    if let Ok(NavigationPreparation::Ready(prepared)) =
                        reader.prepare_navigation(intended_direction)
                    {
                        let raw_progress =
                            (delta.abs() - self.transition_policy.slop_threshold) / width;
                        let progress = raw_progress.clamp(0.0, 1.0);
                        let velocity = gesture.compute_velocity_x();
                        self.transition_state = TransitionState::Interactive {
                            prepared,
                            progress,
                            velocity,
                            gesture: gesture.clone(),
                        };
                        self.dirty = DirtyState::Animation;
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    } else {
                        // Boundary or not ready -> stay idle
                        self.transition_state = TransitionState::Idle;
                        self.active_pointer_id = None;
                    }
                }
            }
            TransitionState::Interactive {
                prepared,
                progress,
                velocity,
                gesture,
            } => {
                gesture.record_move(x);
                let delta = gesture.horizontal_delta();

                let intended_direction = if delta < 0.0 {
                    PageDirection::Next
                } else {
                    PageDirection::Previous
                };

                if prepared.direction() != intended_direction {
                    // Switched drag direction across origin
                    let reader = self.reader.as_mut().unwrap();
                    reader.cancel_navigation(prepared.token());
                    if let Ok(NavigationPreparation::Ready(new_prep)) =
                        reader.prepare_navigation(intended_direction)
                    {
                        *prepared = new_prep;
                    } else {
                        self.transition_state = TransitionState::Idle;
                        self.active_pointer_id = None;
                        self.dirty = DirtyState::Scene;
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        return;
                    }
                }

                let raw_progress =
                    (delta.abs() - self.transition_policy.slop_threshold).max(0.0) / width;
                *progress = raw_progress.clamp(0.0, 1.0);
                *velocity = gesture.compute_velocity_x();

                self.dirty = DirtyState::Animation;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn handle_pointer_up(&mut self, pointer_id: u64) {
        if self.active_pointer_id != Some(pointer_id) {
            return;
        }
        self.active_pointer_id = None;

        match std::mem::replace(&mut self.transition_state, TransitionState::Idle) {
            TransitionState::TrackingSlop { .. } => {
                // Released before exceeding slop threshold -> clean return to idle
                self.dirty = DirtyState::Scene;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            TransitionState::Interactive {
                prepared,
                progress,
                velocity,
                ..
            } => {
                let commit = progress >= self.transition_policy.drag_distance_threshold_ratio
                    || (prepared.direction() == PageDirection::Next
                        && velocity <= -self.transition_policy.fling_velocity_threshold)
                    || (prepared.direction() == PageDirection::Previous
                        && velocity >= self.transition_policy.fling_velocity_threshold);

                let target = if commit { 1.0 } else { 0.0 };
                let duration = self.transition_policy.settle_duration(progress, target);

                self.transition_state = TransitionState::Settling {
                    prepared,
                    from: progress,
                    to: target,
                    started_at: Instant::now(),
                    duration,
                };
                self.dirty = DirtyState::Animation;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn cancel_active_gesture(&mut self) {
        self.active_pointer_id = None;
        match std::mem::replace(&mut self.transition_state, TransitionState::Idle) {
            TransitionState::Interactive { prepared, .. } => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.cancel_navigation(prepared.token());
                }
                self.dirty = DirtyState::Scene;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            TransitionState::TrackingSlop { .. } => {
                self.dirty = DirtyState::Scene;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            other => {
                self.transition_state = other;
            }
        }
    }

    fn handle_key_input(&mut self, event_loop: &ActiveEventLoop, key: KeyCode) {
        match key {
            KeyCode::Escape => {
                self.dump_metrics_if_requested();
                event_loop.exit();
            }
            KeyCode::ArrowRight | KeyCode::Space => {
                self.trigger_keyboard_turn(PageDirection::Next);
            }
            KeyCode::ArrowLeft => {
                self.trigger_keyboard_turn(PageDirection::Previous);
            }
            KeyCode::KeyT => {
                // Toggle transition kind between None and Slide
                self.transition_kind = match self.transition_kind {
                    TransitionKind::None => TransitionKind::Slide,
                    TransitionKind::Slide => TransitionKind::None,
                };
                self.dirty = DirtyState::Scene;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            KeyCode::KeyR => {
                self.scene_cache.clear();
                self.dirty = DirtyState::Scene;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            KeyCode::KeyM => {
                // Simulate critical memory pressure
                self.scene_cache
                    .handle_memory_pressure(MemoryPressure::Critical);
                self.dirty = DirtyState::Scene;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            KeyCode::F1 => {
                self.show_metrics = !self.show_metrics;
                if !self.show_metrics
                    && let Some(w) = &self.window
                {
                    w.set_title("Torto Engine Demo");
                }
                self.dirty = DirtyState::Scene;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for DemoApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("Torto Engine Demo")
            .with_inner_size(LogicalSize::new(800u32, 1000u32))
            .with_min_inner_size(LogicalSize::new(400u32, 500u32));

        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("Failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        self.viewport = size;

        if let Err(e) = self.ensure_reader(size.width.max(1), size.height.max(1)) {
            eprintln!("Initialization error: {e}");
            event_loop.exit();
            return;
        }

        let gpu = match pollster::block_on(SurfaceRenderer::new(Arc::clone(&window))) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("Failed to initialize GPU: {e}");
                event_loop.exit();
                return;
            }
        };

        self.gpu = Some(gpu);
        self.window = Some(window);
        self.dirty = DirtyState::Scene;
    }

    #[allow(clippy::cast_possible_truncation)]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.dump_metrics_if_requested();
                event_loop.exit();
            }
            WindowEvent::Focused(false) => {
                self.cancel_active_gesture();
            }
            WindowEvent::Resized(new_size) => {
                if new_size.width > 0 && new_size.height > 0 {
                    self.cancel_active_gesture();
                    self.viewport = new_size;
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.resize(new_size);
                    }
                    if let Some(reader) = self.reader.as_mut() {
                        let _ = reader.resize(LayoutViewport {
                            width: new_size.width,
                            height: new_size.height,
                        });
                        self.layout_generation += 1;
                        self.scene_cache.clear();
                    }
                    self.dirty = DirtyState::Scene;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if self.dirty != DirtyState::Clean
                    && let Err(e) = self.render_frame()
                {
                    eprintln!("Render error: {e}");
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some(position);
                if self.active_pointer_id == Some(0) {
                    self.handle_pointer_move(0, position.x as f32);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    let x = self.cursor_position.map_or(0.0, |p| p.x as f32);
                    self.handle_pointer_down(0, x);
                }
                ElementState::Released => {
                    self.handle_pointer_up(0);
                }
            },
            WindowEvent::Touch(touch) => match touch.phase {
                winit::event::TouchPhase::Started => {
                    self.handle_pointer_down(touch.id, touch.location.x as f32);
                }
                winit::event::TouchPhase::Moved => {
                    self.handle_pointer_move(touch.id, touch.location.x as f32);
                }
                winit::event::TouchPhase::Ended => {
                    self.handle_pointer_up(touch.id);
                }
                winit::event::TouchPhase::Cancelled => {
                    self.cancel_active_gesture();
                }
            },
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.handle_key_input(event_loop, code);
                }
            }
            _ => {}
        }
    }
}
