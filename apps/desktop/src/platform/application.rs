use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Icon, Theme, Window, WindowId};

use super::UserEvent;
use super::gpu::GpuState;
use crate::app::DesktopApp;
use crate::preferences::AppTheme;

const INITIAL_WIDTH: u32 = 1200;
const INITIAL_HEIGHT: u32 = 800;
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

fn is_fullscreen_toggle(state: ElementState, repeat: bool, logical_key: &Key) -> bool {
    state == ElementState::Pressed && !repeat && logical_key == &NamedKey::F11
}

fn toggle_fullscreen(window: &Window) {
    let fullscreen = window
        .fullscreen()
        .is_none()
        .then_some(Fullscreen::Borderless(None));
    window.set_fullscreen(fullscreen);
    window.request_redraw();
}

const fn native_window_theme(theme: AppTheme) -> Theme {
    match theme {
        AppTheme::Light => Theme::Light,
        AppTheme::Dark => Theme::Dark,
    }
}

struct WindowState {
    window: Arc<Window>,
    gpu: GpuState,
    egui_state: egui_winit::State,
}

struct Application {
    app: DesktopApp,
    egui_ctx: egui::Context,
    window: Option<WindowState>,
    repaint_at: Option<Instant>,
    fatal_error: Option<String>,
    proxy: EventLoopProxy<UserEvent>,
    runtime: tokio::runtime::Runtime,
}

impl Application {
    fn new(
        app: DesktopApp,
        proxy: EventLoopProxy<UserEvent>,
        runtime: tokio::runtime::Runtime,
    ) -> Self {
        let egui_ctx = egui::Context::default();
        crate::ui::configure(&egui_ctx, app.interface_typography(), runtime.handle());
        crate::ui::set_theme(&egui_ctx, app.theme());
        crate::ui::apply_visuals(&egui_ctx, &crate::ui::palette());
        let repaint_proxy = proxy.clone();
        egui_ctx.set_request_repaint_callback(move |request| {
            let _ = repaint_proxy.send_event(UserEvent::RepaintAfter(request.delay));
        });
        Self {
            app,
            egui_ctx,
            window: None,
            repaint_at: None,
            fatal_error: None,
            proxy,
            runtime,
        }
    }

    fn schedule_repaint(&mut self, event_loop: &ActiveEventLoop, delay: Duration) {
        let Some(window) = &self.window else {
            return;
        };
        if delay.is_zero() {
            window.window.request_redraw();
            return;
        }
        let deadline = Instant::now() + delay;
        if self.repaint_at.is_none_or(|current| deadline < current) {
            self.repaint_at = Some(deadline);
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        }
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
            .with_theme(Some(native_window_theme(self.app.theme())))
            .with_inner_size(LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT))
            .with_min_inner_size(LogicalSize::new(720_u32, 520_u32));
        #[cfg(target_os = "windows")]
        let attributes = {
            use winit::platform::windows::WindowAttributesExtWindows as _;

            attributes.with_taskbar_icon(app_icon())
        };
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fatal_error = Some(error.to_string());
                event_loop.exit();
                return;
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
        window.request_redraw();
        self.window = Some(WindowState {
            window,
            gpu,
            egui_state,
        });
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. })
            && self
                .repaint_at
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.repaint_at = None;
            if let Some(window) = &self.window {
                window.window.request_redraw();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::RepaintAfter(delay) => self.schedule_repaint(event_loop, delay),
            #[cfg(target_os = "macos")]
            UserEvent::OpenBook(path) => self.app.open_book(&path),
            #[cfg(target_os = "windows")]
            UserEvent::Update(message) => self.app.complete_update(message),
            UserEvent::ShelfImport(message) => self.app.complete_shelf_import(message),
            UserEvent::ShelfSync(message) => self.app.complete_shelf_sync(message),
            UserEvent::ReaderSearch(message) => self.app.complete_reader_search(message),
            UserEvent::ReaderSemanticIndex(message) => {
                self.app.complete_reader_semantic_index(message);
            }
            UserEvent::ReaderChatStream(message) => self.app.update_reader_chat_stream(message),
            UserEvent::ReaderChat(message) => self.app.complete_reader_chat(message),
            UserEvent::ReaderTranslation(message) => self.app.complete_reader_translation(message),
            UserEvent::ReaderTocTranslation(message) => {
                self.app.complete_reader_toc_translation(message);
            }
            UserEvent::ReaderPdfToc(message) => self.app.complete_reader_pdf_toc(message),
            UserEvent::ReaderPdfOcr(message) => self.app.complete_reader_pdf_ocr(message),
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
        let response = state.egui_state.on_window_event(&state.window, &event);
        if response.repaint {
            state.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if is_fullscreen_toggle(event.state, event.repeat, &event.logical_key) =>
            {
                toggle_fullscreen(&state.window);
            }
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
                state.gpu.resize(size);
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                state.gpu.resize(state.window.inner_size());
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if state.window.inner_size() == PhysicalSize::new(0, 0) {
                    return;
                }
                if let Err(error) = state.gpu.render(
                    &state.window,
                    &mut self.app,
                    &self.egui_ctx,
                    &mut state.egui_state,
                ) {
                    crate::diagnostics::log(
                        "render.fatal",
                        &[crate::diagnostics::Field::Usize(
                            "error_chars",
                            error.chars().count(),
                        )],
                    );
                    self.fatal_error = Some(error);
                    event_loop.exit();
                } else {
                    // A theme switch lands during render; keep the surface
                    // clear color in step for the next frame.
                    state.gpu.set_clear_color(clear_color());
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
        if let Some(deadline) = self.repaint_at {
            if Instant::now() >= deadline {
                self.repaint_at = None;
                if let Some(window) = &self.window {
                    window.window.request_redraw();
                }
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f11_press_toggles_fullscreen_once() {
        let f11 = Key::Named(NamedKey::F11);

        assert!(is_fullscreen_toggle(ElementState::Pressed, false, &f11));
        assert!(!is_fullscreen_toggle(ElementState::Released, false, &f11));
        assert!(!is_fullscreen_toggle(ElementState::Pressed, true, &f11));
        assert!(!is_fullscreen_toggle(
            ElementState::Pressed,
            false,
            &Key::Named(NamedKey::F10),
        ));
    }

    #[test]
    fn initial_window_theme_matches_the_app_theme() {
        assert_eq!(native_window_theme(AppTheme::Light), Theme::Light);
        assert_eq!(native_window_theme(AppTheme::Dark), Theme::Dark);
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
        assert_eq!(wix.matches("Absent='allow'").count(), 2);
        assert!(wix.contains("MigrateFeatures='yes'"));
        assert!(license.contains("Copyright (c) 2026 TortoTech"));
        assert!(installer_license.contains("Copyright (c) 2026 TortoTech"));
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
