use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
#[cfg(target_os = "windows")]
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
#[cfg(target_os = "windows")]
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(not(target_os = "windows"))]
use winit::window::Fullscreen;
use winit::window::{Icon, Theme, Window, WindowId};

use super::UserEvent;
use super::gpu::GpuState;
use crate::app::DesktopApp;
use crate::preferences::AppTheme;

const INITIAL_WIDTH: u32 = 1200;
const INITIAL_HEIGHT: u32 = 800;
#[cfg(target_os = "windows")]
const FULLSCREEN_COMPOSITOR_OVERSCAN: u32 = 1;
#[cfg(target_os = "windows")]
const IME_SHORTCUT_MODIFIER_RELEASE_GRACE: Duration = Duration::from_millis(300);
fn app_icon() -> Option<Icon> {
    let image = image::load_from_memory(include_bytes!("../../../../assets/windows/torto-256.png"))
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).ok()
}

pub(crate) fn run(app: DesktopApp) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    #[cfg(target_os = "macos")]
    let _open_file_handler = rebook_macos_open_file::install({
        let proxy = proxy.clone();
        move |path| {
            let _ = proxy.send_event(UserEvent::OpenBook(path));
        }
    })?;
    let runtime = tokio::runtime::Runtime::new()?;
    let mut application = Application::new(app, proxy, runtime);
    event_loop.run_app(&mut application)?;
    if let Some(error) = application.fatal_error {
        return Err(error.into());
    }
    Ok(())
}

fn clear_color() -> wgpu::Color {
    let background = crate::ui::palette().background;
    wgpu::Color {
        r: f64::from(background.r()) / 255.0,
        g: f64::from(background.g()) / 255.0,
        b: f64::from(background.b()) / 255.0,
        a: 1.0,
    }
}

#[cfg(target_os = "windows")]
fn native_background_color() -> [u8; 3] {
    let background = crate::ui::palette().background;
    [background.r(), background.g(), background.b()]
}

fn toggle_fullscreen(state: &mut WindowState) {
    #[cfg(target_os = "windows")]
    {
        if let Some(placement) = state.windowed_placement.take() {
            state.window.set_decorations(true);
            state.native_frame.set_fullscreen(false);
            state.native_frame.restore_placement(&placement.native);
        } else {
            let Some(monitor) = state.window.current_monitor() else {
                return;
            };
            let Ok(native) = state.native_frame.save_placement() else {
                return;
            };
            let placement = WindowedPlacement { native };
            if state.window.is_maximized() {
                state.window.set_maximized(false);
            }
            // Do not call winit's set_fullscreen on Windows. Besides changing the
            // border and bounds it registers a taskbar/DWM fullscreen window. That
            // special compositor path can invalidate a wgpu flip-model surface when
            // an IME candidate window appears, producing a black frame while typing.
            let (position, size) = compositor_fullscreen_bounds(monitor.position(), monitor.size());
            state.native_frame.set_fullscreen(true);
            state.window.set_decorations(false);
            state.window.set_outer_position(position);
            let _ = state.window.request_inner_size(size);
            state.windowed_placement = Some(placement);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let fullscreen = state
            .window
            .fullscreen()
            .is_none()
            .then_some(Fullscreen::Borderless(None));
        state.window.set_fullscreen(fullscreen);
    }
    state.window.request_redraw();
}

#[cfg(target_os = "windows")]
fn sync_window_chrome(state: &WindowState, ctx: &egui::Context) {
    use crate::app::window_chrome;
    let fullscreen = state.windowed_placement.is_some();
    window_chrome::set_state(
        ctx,
        window_chrome::WindowState {
            fullscreen,
            maximized: state.window.is_maximized(),
            header_hovered: state.native_frame.header_hovered(),
            hovered_button: state.native_frame.hovered_button(),
            pressed_button: state.native_frame.pressed_button(),
        },
    );
    let geometry = window_chrome::geometry(ctx);
    let scale = ctx.pixels_per_point();
    let physical = |rect: egui::Rect| {
        [
            (rect.left() * scale).round() as i32,
            (rect.top() * scale).round() as i32,
            (rect.right() * scale).round() as i32,
            (rect.bottom() * scale).round() as i32,
        ]
    };
    state
        .native_frame
        .set_layout(rebook_windows_window_background::FrameLayout {
            fullscreen,
            header: geometry.header.map(physical),
            excluded: geometry.excluded.into_iter().map(physical).collect(),
            buttons: geometry.buttons.map(|rect| rect.map(physical)),
            drag_enabled: geometry.drag_enabled,
        });
}

#[cfg(target_os = "windows")]
fn compositor_fullscreen_bounds(
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let overscan = FULLSCREEN_COMPOSITOR_OVERSCAN;
    let overscan_i32 = i32::try_from(overscan).unwrap_or(0);
    (
        PhysicalPosition::new(
            monitor_position.x.saturating_sub(overscan_i32),
            monitor_position.y.saturating_sub(overscan_i32),
        ),
        PhysicalSize::new(
            monitor_size
                .width
                .saturating_add(overscan.saturating_mul(2)),
            monitor_size
                .height
                .saturating_add(overscan.saturating_mul(2)),
        ),
    )
}

const fn native_window_theme(theme: AppTheme) -> Option<Theme> {
    match theme {
        AppTheme::System => None,
        AppTheme::Light => Some(Theme::Light),
        AppTheme::Dark => Some(Theme::Dark),
    }
}

#[cfg(target_os = "windows")]
fn native_shortcut_modifiers_match(
    modifiers: ModifiersState,
    shortcut: egui::KeyboardShortcut,
) -> bool {
    let modifiers = egui::Modifiers {
        alt: modifiers.alt_key(),
        ctrl: modifiers.control_key(),
        shift: modifiers.shift_key(),
        mac_cmd: false,
        command: modifiers.control_key(),
    };
    modifiers.matches_logically(shortcut.modifiers)
}

#[cfg(target_os = "windows")]
fn native_open_settings_key_matches(
    key_code: KeyCode,
    repeat: bool,
    modifiers: ModifiersState,
    modifiers_were_just_released: bool,
    shortcut: egui::KeyboardShortcut,
) -> bool {
    if key_code != KeyCode::Comma || shortcut.logical_key != egui::Key::Comma || repeat {
        return false;
    }
    native_shortcut_modifiers_match(modifiers, shortcut) || modifiers_were_just_released
}

#[cfg(target_os = "windows")]
fn native_open_settings_shortcut_matches(
    event: &WindowEvent,
    modifiers: ModifiersState,
    modifiers_were_just_released: bool,
    shortcut: egui::KeyboardShortcut,
) -> bool {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    let PhysicalKey::Code(key_code) = event.physical_key else {
        return false;
    };
    // WeType 2.1 can suppress comma key-down while still emitting key-up.
    // Do not filter by event.state: opening settings is idempotent, so both
    // the normal key-down and the compatibility key-up are safe to accept.
    native_open_settings_key_matches(
        key_code,
        event.repeat,
        modifiers,
        modifiers_were_just_released,
        shortcut,
    )
}

/// Hand a clipboard image to the chat composer, which egui cannot see:
/// egui-winit answers the paste shortcut by pushing `Event::Paste` for text
/// only, so a copied screenshot produces no event and the key never reaches the
/// composer. Both shapes of clipboard image are taken: pixels, and the image
/// files a screenshot tool leaves on the clipboard. Returns whether the
/// shortcut was claimed.
fn claim_pasted_chat_image(
    event: &WindowEvent,
    modifiers: ModifiersState,
    app: &mut DesktopApp,
    ctx: &egui::Context,
) -> bool {
    if !is_paste_shortcut(event, modifiers) || !app.chat_composer_accepts_paste(ctx) {
        return false;
    }
    let images = crate::platform::images_without_text();
    if images.is_empty() {
        return false;
    }
    for image in images {
        app.attach_pasted_chat_image(image);
    }
    true
}

/// The paste shortcut as egui-winit defines it: Ctrl+V (Cmd+V on macOS),
/// Shift+Insert on Windows, and the dedicated Paste key.
fn is_paste_shortcut(event: &WindowEvent, modifiers: ModifiersState) -> bool {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    is_paste_key(
        &event.logical_key,
        event.physical_key,
        event.state,
        event.repeat,
        modifiers,
    )
}

fn is_paste_key(
    logical_key: &Key,
    physical_key: PhysicalKey,
    state: ElementState,
    repeat: bool,
    modifiers: ModifiersState,
) -> bool {
    if state != ElementState::Pressed || repeat {
        return false;
    }
    // Keys the layout does not translate still count through their physical
    // key, matching egui-winit's logical-or-physical fallback.
    let untranslated = matches!(logical_key, Key::Unidentified(_) | Key::Dead(_));
    let character =
        |name: &str| matches!(logical_key, Key::Character(text) if text.eq_ignore_ascii_case(name));
    if *logical_key == Key::Named(NamedKey::Paste) {
        return true;
    }
    let insert = *logical_key == Key::Named(NamedKey::Insert)
        || (untranslated && physical_key == PhysicalKey::Code(KeyCode::Insert));
    if insert {
        return cfg!(target_os = "windows") && modifiers.shift_key();
    }
    let v = character("v") || (untranslated && physical_key == PhysicalKey::Code(KeyCode::KeyV));
    let command = if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    };
    v && command
}

struct WindowState {
    window: Arc<Window>,
    #[cfg(target_os = "windows")]
    native_background: rebook_windows_window_background::WindowBackground,
    #[cfg(target_os = "windows")]
    native_frame: rebook_windows_window_background::WindowFrame,
    gpu: GpuState,
    egui_state: egui_winit::State,
    #[cfg(target_os = "windows")]
    windowed_placement: Option<WindowedPlacement>,
}

#[cfg(target_os = "windows")]
struct WindowedPlacement {
    native: rebook_windows_window_background::WindowPlacement,
}

struct Application {
    app: DesktopApp,
    egui_ctx: egui::Context,
    window: Option<WindowState>,
    repaint: super::repaint::RepaintSchedule,
    fatal_error: Option<String>,
    proxy: EventLoopProxy<UserEvent>,
    runtime: tokio::runtime::Runtime,
    modifiers: ModifiersState,
    #[cfg(target_os = "windows")]
    open_settings_modifiers_released_at: Option<Instant>,
}

impl Application {
    fn new(
        app: DesktopApp,
        proxy: EventLoopProxy<UserEvent>,
        runtime: tokio::runtime::Runtime,
    ) -> Self {
        let egui_ctx = egui::Context::default();
        crate::ui::configure(
            &egui_ctx,
            app.interface_typography(),
            app.interface_language(),
            runtime.handle(),
        );
        crate::ui::set_theme(&egui_ctx, app.theme());
        crate::ui::apply_visuals(&egui_ctx, &crate::ui::palette());
        let repaint_proxy = proxy.clone();
        egui_ctx.set_request_repaint_callback(move |request| {
            if let Some(when) = Instant::now().checked_add(request.delay) {
                let _ = repaint_proxy.send_event(UserEvent::EguiRepaint {
                    when,
                    cumulative_pass_nr: request.current_cumulative_pass_nr,
                    viewport_id: request.viewport_id,
                });
            }
        });
        Self {
            app,
            egui_ctx,
            window: None,
            repaint: super::repaint::RepaintSchedule::default(),
            fatal_error: None,
            proxy,
            runtime,
            modifiers: ModifiersState::default(),
            #[cfg(target_os = "windows")]
            open_settings_modifiers_released_at: None,
        }
    }

    fn update_repaint_schedule(&mut self, event_loop: &ActiveEventLoop) {
        let current = self.egui_ctx.cumulative_pass_nr_for(egui::ViewportId::ROOT);
        if self.repaint.take_due(Instant::now(), current)
            && let Some(window) = &self.window
        {
            window.window.request_redraw();
        }
        event_loop.set_control_flow(match self.repaint.next_deadline(current) {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }

    fn render_window_state(
        state: &mut WindowState,
        app: &mut DesktopApp,
        egui_ctx: &egui::Context,
    ) {
        #[cfg(target_os = "windows")]
        sync_window_chrome(state, egui_ctx);
        if let Err(error) = state
            .gpu
            .render(&state.window, app, egui_ctx, &mut state.egui_state)
        {
            tracing::warn!(%error, "failed to present resized window frame");
            state.window.request_redraw();
        }
        #[cfg(target_os = "windows")]
        sync_window_chrome(state, egui_ctx);
    }
}

impl ApplicationHandler<UserEvent> for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("Torto")
            .with_window_icon(app_icon())
            .with_theme(native_window_theme(self.app.theme()))
            .with_inner_size(LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT))
            .with_min_inner_size(LogicalSize::new(720_u32, 520_u32));
        #[cfg(target_os = "windows")]
        let attributes = {
            use winit::platform::windows::WindowAttributesExtWindows as _;

            // Windows can display unspecified client pixels as soon as a visible
            // HWND is created, while adapter/device initialization still takes
            // place. Install the themed native background and prepare the GPU
            // before exposing the window to DWM.
            attributes.with_taskbar_icon(app_icon()).with_visible(false)
        };
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fatal_error = Some(error.to_string());
                event_loop.exit();
                return;
            }
        };
        crate::smoke::stage("window-created");
        #[cfg(target_os = "windows")]
        let (native_background, native_frame) = {
            let hwnd = match window.window_handle().map(|handle| handle.as_raw()) {
                Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
                Ok(_) => {
                    self.fatal_error = Some("当前窗口没有可用的 Win32 句柄".to_owned());
                    event_loop.exit();
                    return;
                }
                Err(error) => {
                    self.fatal_error = Some(error.to_string());
                    event_loop.exit();
                    return;
                }
            };
            match rebook_windows_window_background::WindowBackground::install(
                hwnd,
                native_background_color(),
            ) {
                Ok(background) => {
                    match rebook_windows_window_background::WindowFrame::install(hwnd) {
                        Ok(frame) => (background, frame),
                        Err(error) => {
                            self.fatal_error = Some(error.to_string());
                            event_loop.exit();
                            return;
                        }
                    }
                }
                Err(error) => {
                    self.fatal_error = Some(error.to_string());
                    event_loop.exit();
                    return;
                }
            }
        };
        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            None,
            window.theme(),
            None,
        );
        let gpu = match pollster::block_on(GpuState::new(Arc::clone(&window))) {
            Ok(gpu) => gpu,
            Err(error) => {
                self.fatal_error = Some(error);
                event_loop.exit();
                return;
            }
        };
        let mut gpu = gpu;
        gpu.set_clear_color(clear_color());
        let state = WindowState {
            window,
            #[cfg(target_os = "windows")]
            native_background,
            #[cfg(target_os = "windows")]
            native_frame,
            gpu,
            egui_state,
            #[cfg(target_os = "windows")]
            windowed_placement: None,
        };
        #[cfg(target_os = "windows")]
        let state = {
            let mut state = state;
            state.window.set_visible(true);
            let size = state.window.inner_size();
            crate::diagnostics::log(
                "window.startup_reveal",
                &[
                    crate::diagnostics::Field::U64("width", u64::from(size.width)),
                    crate::diagnostics::Field::U64("height", u64::from(size.height)),
                ],
            );
            // Present synchronously in the same handler that reveals the HWND.
            // The queued redraw covers drivers that initially report the surface
            // as occluded during the visibility transition.
            Self::render_window_state(&mut state, &mut self.app, &self.egui_ctx);
            state.window.request_redraw();
            state
        };
        #[cfg(not(target_os = "windows"))]
        let state = {
            state.window.request_redraw();
            state
        };
        self.window = Some(state);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if crate::smoke::enabled() && matches!(cause, StartCause::ResumeTimeReached { .. }) {
            if let Some(window) = &self.window {
                window.window.request_redraw();
            }
        }
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            self.update_repaint_schedule(event_loop);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let sync_callback = match &event {
            UserEvent::ShelfSync(_) => Some("sync.complete"),
            UserEvent::ShelfSyncProgress(_) => Some("sync.progress"),
            _ => None,
        };
        let callback_started = Instant::now();
        match event {
            UserEvent::RepaintAfter(delay) => {
                if let Some(when) = Instant::now().checked_add(delay) {
                    self.repaint.external(when);
                    self.update_repaint_schedule(event_loop);
                }
                return;
            }
            UserEvent::EguiRepaint {
                when,
                cumulative_pass_nr,
                viewport_id,
            } => {
                if viewport_id == egui::ViewportId::ROOT {
                    let current = self.egui_ctx.cumulative_pass_nr_for(viewport_id);
                    self.repaint.egui(when, cumulative_pass_nr, current);
                    self.update_repaint_schedule(event_loop);
                }
                return;
            }
            UserEvent::ShelfSyncCheck(message) => {
                if !self.app.complete_shelf_sync_check(message) {
                    return;
                }
            }
            #[cfg(target_os = "macos")]
            UserEvent::OpenBook(path) => self.app.open_book(&path),
            #[cfg(target_os = "windows")]
            UserEvent::Update(message) => self.app.complete_update(message),
            UserEvent::ShelfImport(message) => self.app.complete_shelf_import(message),
            UserEvent::ShelfOpen(message) => self.app.complete_shelf_open(message, &self.runtime),
            UserEvent::ShelfOpenHeader(message) => self.app.complete_shelf_open_header(message),
            UserEvent::ReaderRendererReady(message) => {
                if let Some(state) = self.window.as_mut() {
                    state
                        .gpu
                        .complete_prewarm(message, self.app.reader_expected(), &self.runtime);
                } else {
                    self.runtime.spawn_blocking(move || drop(message));
                }
            }
            UserEvent::ShelfSyncProgress(message) => self.app.update_shelf_sync_progress(message),
            UserEvent::ShelfSync(message) => self.app.complete_shelf_sync(message),
            UserEvent::SettingsProviderModels(message) => {
                self.app.complete_settings_provider_models(message);
            }
            UserEvent::ReaderSearch(message) => self.app.complete_reader_search(message),
            UserEvent::ReaderChatStream(message) => self.app.update_reader_chat_stream(message),
            UserEvent::ReaderChat(message) => self.app.complete_reader_chat(message),
            UserEvent::ReaderTranslation(message) => self.app.complete_reader_translation(message),
            UserEvent::ReaderTocTranslation(message) => {
                self.app.complete_reader_toc_translation(message);
            }
            UserEvent::ReaderPdfToc(message) => self.app.complete_reader_pdf_toc(message),
            UserEvent::ReaderPdfOcr(message) => self.app.complete_reader_pdf_ocr(message),
            UserEvent::ReaderPdfOriginal(message) => self.app.complete_reader_pdf_original(message),
            UserEvent::ReaderPdfNative(message) => self.app.complete_reader_pdf_native(message),
        }
        if let Some(callback) = sync_callback
            && callback_started.elapsed() >= Duration::from_millis(100)
        {
            crate::diagnostics::log(
                "window.slow_sync_callback",
                &[
                    crate::diagnostics::Field::Text("callback", callback),
                    crate::diagnostics::Field::U64(
                        "elapsed_ms",
                        callback_started.elapsed().as_millis() as u64,
                    ),
                ],
            );
        }
        if let Some(window) = &self.window {
            window.window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.window.as_mut() else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }
        #[cfg(target_os = "windows")]
        {
            let open_settings_shortcut = self.app.open_settings_shortcut();
            if let WindowEvent::ModifiersChanged(modifiers) = &event {
                let matched_before =
                    native_shortcut_modifiers_match(self.modifiers, open_settings_shortcut);
                self.modifiers = modifiers.state();
                let matches_now =
                    native_shortcut_modifiers_match(self.modifiers, open_settings_shortcut);
                if open_settings_shortcut.logical_key == egui::Key::Comma
                    && open_settings_shortcut.modifiers != egui::Modifiers::NONE
                    && matched_before
                    && !matches_now
                {
                    self.open_settings_modifiers_released_at = Some(Instant::now());
                } else if matches_now {
                    self.open_settings_modifiers_released_at = None;
                }
            }
            let modifiers_were_just_released = self
                .open_settings_modifiers_released_at
                .is_some_and(|released_at| {
                    released_at.elapsed() <= IME_SHORTCUT_MODIFIER_RELEASE_GRACE
                });
            if self.open_settings_modifiers_released_at.is_some() && !modifiers_were_just_released {
                self.open_settings_modifiers_released_at = None;
            }
            if native_open_settings_shortcut_matches(
                &event,
                self.modifiers,
                modifiers_were_just_released,
                open_settings_shortcut,
            ) {
                // Handle this before egui so WeType's missing key-down can fall
                // back to the physical comma key-up event.
                self.app.open_settings();
                self.open_settings_modifiers_released_at = None;
                state.window.request_redraw();
            }
        }
        #[cfg(not(target_os = "windows"))]
        if let WindowEvent::ModifiersChanged(modifiers) = &event {
            self.modifiers = modifiers.state();
        }
        if claim_pasted_chat_image(&event, self.modifiers, &mut self.app, &self.egui_ctx) {
            // The composer claimed the shortcut, so egui must not also read the
            // clipboard; the new attachment needs a frame to appear.
            state.window.request_redraw();
            return;
        }
        let response = state.egui_state.on_window_event(&state.window, &event);
        // RedrawRequested is already being serviced below. egui-winit marks
        // it as needing paint, but requesting another frame here creates a
        // perpetual render loop even when egui has no pending repaint.
        if response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
            state.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Focused(focused) => {
                let size = state.window.inner_size();
                crate::diagnostics::log(
                    "window.focus",
                    &[
                        crate::diagnostics::Field::Bool("focused", focused),
                        crate::diagnostics::Field::U64("width", u64::from(size.width)),
                        crate::diagnostics::Field::U64("height", u64::from(size.height)),
                    ],
                );
                self.app
                    .log_reader_diagnostics("window.focus.reader", Some(focused));
                if focused {
                    state.window.request_redraw();
                }
            }
            WindowEvent::Occluded(occluded) => {
                crate::diagnostics::log(
                    "window.occluded",
                    &[crate::diagnostics::Field::Bool("occluded", occluded)],
                );
                self.app
                    .log_reader_diagnostics("window.occluded.reader", None);
            }
            WindowEvent::Resized(size) => {
                let minimized = state.window.is_minimized() == Some(true);
                if minimized || size.width == 0 || size.height == 0 {
                    crate::diagnostics::log(
                        "window.minimize",
                        &[
                            crate::diagnostics::Field::Bool("observed", minimized),
                            crate::diagnostics::Field::U64("width", u64::from(size.width)),
                            crate::diagnostics::Field::U64("height", u64::from(size.height)),
                        ],
                    );
                }
                state.gpu.resize(size);
                // Rendering first presents the retained UI over the themed
                // background, before laying out content at the new size.
                Self::render_window_state(state, &mut self.app, &self.egui_ctx);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                state.gpu.resize(state.window.inner_size());
                Self::render_window_state(state, &mut self.app, &self.egui_ctx);
            }
            WindowEvent::RedrawRequested => {
                self.repaint.on_redraw(
                    Instant::now(),
                    self.egui_ctx.cumulative_pass_nr_for(egui::ViewportId::ROOT),
                );
                if !super::gpu::window_can_render(&state.window) {
                    return;
                }
                #[cfg(target_os = "windows")]
                sync_window_chrome(state, &self.egui_ctx);
                if let Err(error) = state.gpu.render(
                    &state.window,
                    &mut self.app,
                    &self.egui_ctx,
                    &mut state.egui_state,
                ) {
                    crate::diagnostics::log(
                        "render.fatal",
                        &[
                            crate::diagnostics::Field::Usize("error_chars", error.chars().count()),
                            crate::diagnostics::Field::Detail("error", &error),
                            crate::diagnostics::Field::U64(
                                "width",
                                u64::from(state.window.inner_size().width),
                            ),
                            crate::diagnostics::Field::U64(
                                "height",
                                u64::from(state.window.inner_size().height),
                            ),
                        ],
                    );
                    self.fatal_error = Some(error);
                    event_loop.exit();
                } else {
                    #[cfg(target_os = "windows")]
                    {
                        sync_window_chrome(state, &self.egui_ctx);
                        if crate::app::window_chrome::take_close_request(&self.egui_ctx) {
                            event_loop.exit();
                        }
                    }
                    // A theme switch lands during render; keep the surface
                    // clear color in step for the next frame.
                    state.gpu.set_clear_color(clear_color());
                    #[cfg(target_os = "windows")]
                    state.native_background.set_color(native_background_color());
                    if self.app.take_fullscreen_toggle_request() {
                        toggle_fullscreen(state);
                    }
                    state
                        .gpu
                        .prewarm_reader(&self.app, &self.runtime, &self.proxy);
                    self.app.spawn_pending_tasks(&self.runtime, &self.proxy);
                    #[cfg(target_os = "windows")]
                    if let Some(request) = self.app.take_update_install_request() {
                        match crate::updater::launch_installer_after_exit(&request) {
                            Ok(()) => event_loop.exit(),
                            Err(error) => {
                                self.app.report_update_install_error(request, error);
                                state.window.request_redraw();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if crate::smoke::enabled() {
            if let Some(state) = &self.window
                && let Err(error) = crate::smoke::window_tick(&self.egui_ctx, &state.window)
            {
                self.fatal_error = Some(error);
                event_loop.exit();
                return;
            }
            match crate::smoke::should_exit() {
                Ok(true) => {
                    event_loop.exit();
                    return;
                }
                Err(error) => {
                    self.fatal_error = Some(error);
                    event_loop.exit();
                    return;
                }
                Ok(false) => {}
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(100),
            ));
            return;
        }
        self.update_repaint_schedule(event_loop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_shortcuts_match_what_egui_winit_swallows() {
        let pressed = ElementState::Pressed;
        let released = ElementState::Released;
        let key = |text: &str| -> Key { Key::Character(text.into()) };
        let v = PhysicalKey::Code(KeyCode::KeyV);
        let command = if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        };
        let shift = ModifiersState::SHIFT;
        let none = ModifiersState::empty();

        assert!(is_paste_key(&key("v"), v, pressed, false, command));
        assert!(is_paste_key(&key("V"), v, pressed, false, command));
        assert!(is_paste_key(
            &Key::Named(NamedKey::Paste),
            PhysicalKey::Code(KeyCode::F13),
            pressed,
            false,
            none,
        ));
        #[cfg(target_os = "windows")]
        assert!(is_paste_key(
            &Key::Named(NamedKey::Insert),
            PhysicalKey::Code(KeyCode::Insert),
            pressed,
            false,
            shift,
        ));
        // Anything else keeps its normal meaning, so egui still sees it.
        assert!(!is_paste_key(&key("v"), v, pressed, false, none));
        assert!(!is_paste_key(&key("v"), v, released, false, command));
        assert!(!is_paste_key(&key("v"), v, pressed, true, command));
        assert!(!is_paste_key(
            &key("c"),
            PhysicalKey::Code(KeyCode::KeyC),
            pressed,
            false,
            command,
        ));
        // A layout that translates the key elsewhere keeps that meaning.
        assert!(!is_paste_key(&key("x"), v, pressed, false, command));
        // An untranslated key still counts through its physical position.
        assert!(is_paste_key(&Key::Dead(None), v, pressed, false, command));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn compositor_fullscreen_overscans_the_monitor_by_one_pixel() {
        assert_eq!(
            compositor_fullscreen_bounds(
                PhysicalPosition::new(100, -200),
                PhysicalSize::new(2560, 1440),
            ),
            (
                PhysicalPosition::new(99, -201),
                PhysicalSize::new(2562, 1442),
            )
        );
    }

    #[test]
    fn initial_window_theme_matches_the_app_theme() {
        assert_eq!(native_window_theme(AppTheme::System), None);
        assert_eq!(native_window_theme(AppTheme::Light), Some(Theme::Light));
        assert_eq!(native_window_theme(AppTheme::Dark), Some(Theme::Dark));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_comma_shortcut_bypasses_ime_processed_logical_keys() {
        let shortcut = egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Comma);
        let modifiers = ModifiersState::CONTROL;

        assert!(native_open_settings_key_matches(
            KeyCode::Comma,
            false,
            modifiers,
            false,
            shortcut,
        ));
        assert!(!native_open_settings_key_matches(
            KeyCode::Period,
            false,
            modifiers,
            false,
            shortcut,
        ));
        assert!(!native_open_settings_key_matches(
            KeyCode::Comma,
            false,
            ModifiersState::empty(),
            false,
            shortcut,
        ));
        assert!(native_open_settings_key_matches(
            KeyCode::Comma,
            false,
            ModifiersState::empty(),
            true,
            shortcut,
        ));
        assert!(!native_open_settings_key_matches(
            KeyCode::Period,
            false,
            ModifiersState::empty(),
            true,
            shortcut,
        ));
    }

    #[test]
    fn installer_keeps_shortcut_icons_and_remembers_the_install_location() {
        let wix = include_str!("../../wix/main.wxs");
        assert_eq!(wix.matches("Icon='ProductIcon.exe'").count(), 2);
        assert!(wix.contains("<Property Id='APPLICATIONFOLDER' Secure='yes'>"));
        assert!(wix.contains("Id='PreviousApplicationFolder'"));
        assert!(wix.contains("Id='LegacyApplicationFolder'"));
        assert!(wix.contains("Value='[LEGACYAPPLICATIONFOLDER]'"));
        assert!(wix.contains("Value='[APPLICATIONFOLDER]'"));
        assert!(wix.contains("<ComponentRef Id='InstallLocationRegistry'/>"));
    }

    #[test]
    fn installer_registers_supported_books_with_windows_default_apps() {
        let wix = include_str!("../../wix/main.wxs");

        assert!(wix.contains("Key='Software\\RegisteredApplications'"));
        assert!(wix.contains("Value='Software\\TortoTech\\Torto\\Capabilities'"));
        assert!(wix.contains("<ComponentRef Id='FileAssociations'/>"));
        assert!(wix.contains("Id='FileAssociationsFeature'"));
        assert!(wix.contains("Title='E-book file associations'"));
        assert!(wix.contains("Value='&quot;[APPLICATIONFOLDER]torto.exe&quot; &quot;%1&quot;'"));
        for extension in [
            "epub", "mobi", "azw", "azw3", "fb2", "fbz", "cbz", "chm", "pdf",
        ] {
            assert!(
                wix.contains(&format!(
                    "Name='.{extension}' Type='string' Value='Torto.Book'"
                )),
                "missing default-app capability for .{extension}"
            );
            assert!(
                wix.contains(&format!("Key='.{extension}\\OpenWithProgids'")),
                "missing Open With registration for .{extension}"
            );
        }
    }

    #[test]
    fn installer_brand_and_desktop_shortcut_are_configurable() {
        let wix = include_str!("../../wix/main.wxs");
        let license = include_str!("../../../../LICENSE");
        let installer_license = include_str!("../../wix/License.rtf");

        assert!(wix.contains("Manufacturer='TortoTech'"));
        assert!(!wix.contains("Manufacturer='L-Chris'"));
        assert!(wix.contains("Id='DesktopShortcutFeature'"));
        assert!(wix.contains("Title='Desktop shortcut'"));
        assert!(wix.contains("<ComponentRef Id='DesktopShortcutComponent'/>"));
        assert!(wix.contains("<Directory Id='DesktopFolder'>"));
        assert!(!wix.contains("<Directory Id='CommonDesktopFolder'"));
        let desktop_shortcut = wix
            .split("<Component Id='DesktopShortcutComponent'")
            .nth(1)
            .and_then(|component| component.split("</Component>").next())
            .expect("desktop shortcut component");
        assert!(desktop_shortcut.contains("Root='HKCU'"));
        assert!(desktop_shortcut.contains("Key='Software\\TortoTech\\Torto'"));
        assert!(desktop_shortcut.contains("Name='DesktopShortcut'"));
        assert_eq!(wix.matches("Absent='allow'").count(), 2);
        assert!(wix.contains("MigrateFeatures='yes'"));
        assert!(license.contains("Copyright (c) 2026 TortoTech"));
        assert!(installer_license.contains("Copyright (c) 2026 TortoTech"));
        for text in [license, installer_license] {
            assert!(text.contains("SPDX-License-Identifier: AGPL-3.0-only"));
            assert!(text.contains("GNU AFFERO GENERAL PUBLIC LICENSE"));
            assert!(text.contains("13. Remote Network Interaction"));
        }
    }

    #[test]
    fn macos_bundle_declares_supported_book_document_types() {
        let manifest = include_str!("../../Cargo.toml");
        let document_types = include_str!("../../../../assets/macos/document-types.plist");

        assert!(manifest.contains("osx_info_plist_exts = [\"assets/macos/document-types.plist\"]"));
        assert!(document_types.contains("<key>CFBundleDocumentTypes</key>"));
        assert!(document_types.contains("<string>org.idpf.epub-container</string>"));
        assert!(document_types.contains("<string>com.adobe.pdf</string>"));
        for extension in ["mobi", "azw", "azw3", "fb2", "fbz", "cbz", "chm"] {
            assert!(
                document_types.contains(&format!("<string>{extension}</string>")),
                "missing macOS document declaration for .{extension}"
            );
        }
    }
}
