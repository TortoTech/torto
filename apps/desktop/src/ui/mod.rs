mod http_loader;
mod icons;
mod svg_loader;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use egui::emath::GuiRounding;
use egui::{
    Align2, Color32, ColorImage, CornerRadius, FontData, FontDefinitions, FontFamily, Rect,
    Response, RichText, Sense, Stroke, TextStyle, Ui, Vec2, WidgetInfo, WidgetType,
};

pub(crate) use icons::{Icon, IconWidget, paint_icon};

use crate::preferences::{
    AppTheme, DEFAULT_INTERFACE_FONT_SIZE, InterfaceTypography, SYSTEM_INTERFACE_FONT,
};

const EGUI_BASE_FONT_SIZE: f32 = 13.0;
const EGUI_BASE_EXTRA_TEXT_LINE_SPACING: f32 = 1.0;
const TOAST_MAX_WIDTH: f32 = 400.0;
const TOAST_MIN_WIDTH: f32 = 96.0;
const TOAST_SCREEN_MARGIN: f32 = 48.0;
const TOAST_HORIZONTAL_PADDING: f32 = 28.0;
const TOAST_ICON_SIZE: f32 = 18.0;
const TOAST_ITEM_SPACING: f32 = 10.0;
const TOAST_CLOSE_SIZE: f32 = 28.0;
static INTERFACE_FONT_SIZE_BITS: AtomicU32 = AtomicU32::new(DEFAULT_INTERFACE_FONT_SIZE.to_bits());

#[derive(Clone, Copy)]
pub(crate) enum ToastKind {
    Success,
    Error,
    Loading,
}

/// Theme-dependent color set. Chrome reads colors through `palette()` so a
/// saved theme switch recolors the whole app without threading state through
/// every view.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) dark: bool,
    pub(crate) background: Color32,
    pub(crate) surface: Color32,
    pub(crate) surface_muted: Color32,
    pub(crate) text: Color32,
    pub(crate) muted: Color32,
    pub(crate) border: Color32,
    pub(crate) accent: Color32,
    pub(crate) accent_soft: Color32,
    pub(crate) hovered_fill: Color32,
    pub(crate) hovered_weak_fill: Color32,
    pub(crate) hovered_stroke: Color32,
    pub(crate) active_fill: Color32,
    pub(crate) active_weak_fill: Color32,
    pub(crate) open_fill: Color32,
    pub(crate) selection_fill: Color32,
    pub(crate) error: Color32,
    pub(crate) error_fill: Color32,
    pub(crate) error_stroke: Color32,
    pub(crate) error_text: Color32,
    pub(crate) card_fill: Color32,
    pub(crate) accent_border: Color32,
    pub(crate) pill_fill: Color32,
    pub(crate) pill_stroke: Color32,
}

impl Palette {
    fn light() -> Self {
        Self {
            dark: false,
            background: Color32::from_rgb(246, 244, 239),
            surface: Color32::from_rgb(255, 255, 255),
            surface_muted: Color32::from_rgb(240, 238, 233),
            text: Color32::from_rgb(38, 38, 36),
            muted: Color32::from_rgb(118, 116, 109),
            border: Color32::from_rgb(218, 215, 207),
            accent: Color32::from_rgb(68, 137, 103),
            accent_soft: Color32::from_rgb(222, 237, 228),
            hovered_fill: Color32::from_rgb(231, 235, 228),
            hovered_weak_fill: Color32::from_rgb(237, 240, 235),
            hovered_stroke: Color32::from_rgb(171, 184, 174),
            active_fill: Color32::from_rgb(219, 229, 221),
            active_weak_fill: Color32::from_rgb(228, 237, 230),
            open_fill: Color32::from_rgb(237, 240, 235),
            selection_fill: Color32::from_rgba_unmultiplied(68, 137, 103, 64),
            error: Color32::from_rgb(180, 55, 55),
            error_fill: Color32::from_rgb(252, 239, 238),
            error_stroke: Color32::from_rgb(226, 180, 176),
            error_text: Color32::from_rgb(151, 54, 50),
            card_fill: Color32::from_rgb(251, 250, 247),
            accent_border: Color32::from_rgb(177, 209, 190),
            pill_fill: Color32::from_rgb(231, 235, 242),
            pill_stroke: Color32::from_rgb(220, 223, 228),
        }
    }

    fn dark() -> Self {
        Self {
            dark: true,
            background: Color32::from_rgb(32, 31, 28),
            surface: Color32::from_rgb(42, 41, 37),
            surface_muted: Color32::from_rgb(52, 50, 45),
            text: Color32::from_rgb(232, 230, 225),
            muted: Color32::from_rgb(150, 147, 138),
            border: Color32::from_rgb(64, 62, 56),
            accent: Color32::from_rgb(88, 148, 114),
            accent_soft: Color32::from_rgb(44, 62, 52),
            hovered_fill: Color32::from_rgb(52, 57, 51),
            hovered_weak_fill: Color32::from_rgb(46, 49, 44),
            hovered_stroke: Color32::from_rgb(90, 102, 92),
            active_fill: Color32::from_rgb(48, 58, 51),
            active_weak_fill: Color32::from_rgb(44, 54, 48),
            open_fill: Color32::from_rgb(46, 49, 44),
            selection_fill: Color32::from_rgba_unmultiplied(88, 148, 114, 72),
            error: Color32::from_rgb(219, 120, 111),
            error_fill: Color32::from_rgb(64, 40, 38),
            error_stroke: Color32::from_rgb(110, 64, 60),
            error_text: Color32::from_rgb(224, 138, 130),
            card_fill: Color32::from_rgb(47, 46, 42),
            accent_border: Color32::from_rgb(62, 94, 78),
            pill_fill: Color32::from_rgb(54, 53, 48),
            pill_stroke: Color32::from_rgb(72, 70, 64),
        }
    }
}

static DARK_THEME: AtomicBool = AtomicBool::new(false);
static THEME_PREFERENCE: AtomicU8 = AtomicU8::new(0);

pub(crate) fn set_theme(ctx: &egui::Context, theme: AppTheme) {
    THEME_PREFERENCE.store(theme_preference_value(theme), Ordering::Relaxed);
    ctx.set_theme(egui_theme_preference(theme));
    sync_resolved_theme(ctx);
}

const fn egui_theme_preference(theme: AppTheme) -> egui::ThemePreference {
    match theme {
        AppTheme::System => egui::ThemePreference::System,
        AppTheme::Dark => egui::ThemePreference::Dark,
        AppTheme::Light => egui::ThemePreference::Light,
    }
}

const fn theme_preference_value(theme: AppTheme) -> u8 {
    match theme {
        AppTheme::System => 0,
        AppTheme::Light => 1,
        AppTheme::Dark => 2,
    }
}

pub(crate) fn theme_preference() -> AppTheme {
    match THEME_PREFERENCE.load(Ordering::Relaxed) {
        1 => AppTheme::Light,
        2 => AppTheme::Dark,
        _ => AppTheme::System,
    }
}

pub(crate) fn sync_system_theme(ctx: &egui::Context, preference: AppTheme) -> bool {
    preference == AppTheme::System && sync_resolved_theme(ctx)
}

fn sync_resolved_theme(ctx: &egui::Context) -> bool {
    let dark = ctx.theme() == egui::Theme::Dark;
    DARK_THEME.swap(dark, Ordering::Relaxed) != dark
}

pub(crate) fn theme() -> egui::Theme {
    if DARK_THEME.load(Ordering::Relaxed) {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    }
}

pub(crate) fn palette() -> Palette {
    match theme() {
        egui::Theme::Light => Palette::light(),
        egui::Theme::Dark => Palette::dark(),
    }
}

pub(crate) fn configure(
    ctx: &egui::Context,
    interface_typography: &InterfaceTypography,
    runtime: &tokio::runtime::Handle,
) {
    egui_extras::install_image_loaders(ctx);
    http_loader::install(ctx, runtime);
    svg_loader::install(ctx);
    // Application state is mutated while building a frame. A single pass keeps
    // keyboard and pointer actions exactly-once; the retained reader layout
    // performs its own explicit invalidation when geometry changes.
    ctx.options_mut(|options| {
        options.max_passes = 1.try_into().expect("one is non-zero");
        options.sync_window_theme = true;
    });
    configure_tessellation(ctx);
    apply_interface_typography(ctx, interface_typography);

    apply_visuals(ctx, &Palette::light());
    ctx.all_styles_mut(|style| {
        // Application chrome should not behave like selectable document text.
        // Reader text selection is handled by the Vello-backed reader itself.
        style.interaction.selectable_labels = false;
        // egui 0.36's debug-only rect/id diagnostic has known false positives for
        // right-to-left child layouts and virtualized/animated regions (#8343,
        // #8092), painting bright red boxes into otherwise valid frames.
        #[cfg(debug_assertions)]
        {
            style.debug.warn_if_rect_changes_id = false;
        }
        // The default edge fades look like detached gray bars on a solid sidebar.
        style.spacing.scroll.fade.strength = 0.0;
        // Use the same soft surface/accent palette as the rest of the app. The
        // egui floating preset otherwise paints a dragged handle with the dark
        // foreground color, which looks almost black in the light theme.
        let scroll = &mut style.spacing.scroll;
        scroll.foreground_color = false;
        scroll.bar_width = 8.0;
        scroll.floating_width = 3.0;
        scroll.handle_min_length = 28.0;
        scroll.active_background_opacity = 0.18;
        scroll.interact_background_opacity = 0.32;
        scroll.active_handle_opacity = 0.72;
        scroll.interact_handle_opacity = 0.95;
    });
}

fn configure_tessellation(ctx: &egui::Context) {
    // Snap rectangular geometry to pixels, but retain a one-pixel antialiasing
    // fringe for curved corners. Disabling feathering makes small-radius controls
    // visibly stair-step at 100% Windows DPI.
    ctx.tessellation_options_mut(|options| {
        options.feathering = true;
        options.feathering_size_in_pixels = 1.0;
        options.round_rects_to_pixels = true;
    });
}

pub(crate) fn apply_interface_typography(
    ctx: &egui::Context,
    interface_typography: &InterfaceTypography,
) {
    let mut interface_typography = interface_typography.clone();
    interface_typography.normalize();
    INTERFACE_FONT_SIZE_BITS.store(interface_typography.font_size.to_bits(), Ordering::Relaxed);
    let extra_text_line_spacing = interface_extra_text_line_spacing(interface_typography.font_size);

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "reader-cjk".into(),
        FontData::from_static(crate::fonts::cjk_font_bytes()).into(),
    );
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    let requested_families = if interface_typography.font_family == SYSTEM_INTERFACE_FONT {
        system_ui_font_candidates()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    } else {
        std::iter::once(interface_typography.font_family.clone())
            .chain(system_ui_font_candidates().iter().map(ToString::to_string))
            .collect()
    };
    let mut interface_fonts = Vec::new();
    let mut interface_bold_fonts = Vec::new();
    for family in requested_families {
        if interface_fonts.iter().any(|loaded| loaded == &family) {
            continue;
        }
        if let Some(key) = load_system_font(
            &database,
            &family,
            fontdb::Weight::NORMAL,
            "regular",
            &mut fonts,
        ) {
            interface_fonts.push(key);
        }
        if let Some(key) =
            load_system_font(&database, &family, fontdb::Weight::BOLD, "bold", &mut fonts)
        {
            interface_bold_fonts.push(key);
        }
    }
    let proportional_fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    interface_fonts.extend(proportional_fallbacks);
    interface_fonts.push("reader-cjk".into());
    interface_fonts.dedup();
    fonts
        .families
        .insert(FontFamily::Proportional, interface_fonts.clone());
    interface_bold_fonts.extend(interface_fonts);
    interface_bold_fonts.dedup();
    fonts.families.insert(
        FontFamily::Name(egui_commonmark_backend::STRONG_FONT_FAMILY.into()),
        interface_bold_fonts,
    );

    let monospace_fonts = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace_fonts.push("reader-cjk".into());
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = egui::style::default_text_styles();
        for font_id in style.text_styles.values_mut() {
            font_id.size = scaled_font_size(font_id.size);
        }
        style.spacing.extra_text_line_spacing = extra_text_line_spacing;
    });
    ctx.request_repaint();
}

pub(crate) fn scaled_font_size(nominal_size: f32) -> f32 {
    let configured = f32::from_bits(INTERFACE_FONT_SIZE_BITS.load(Ordering::Relaxed));
    nominal_size * configured / EGUI_BASE_FONT_SIZE
}

fn interface_extra_text_line_spacing(font_size: f32) -> f32 {
    EGUI_BASE_EXTRA_TEXT_LINE_SPACING * font_size / EGUI_BASE_FONT_SIZE
}

pub(crate) fn available_interface_font_families() -> Vec<String> {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    let mut families = database
        .faces()
        .flat_map(|face| face.families.iter().map(|(family, _)| family.clone()))
        .filter(|family| !family.trim().is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    families.retain(|family| family != SYSTEM_INTERFACE_FONT);
    families.insert(0, SYSTEM_INTERFACE_FONT.into());
    families
}

fn load_system_font(
    database: &fontdb::Database,
    family: &str,
    weight: fontdb::Weight,
    variant: &str,
    fonts: &mut FontDefinitions,
) -> Option<String> {
    let families = [fontdb::Family::Name(family)];
    let id = database.query(&fontdb::Query {
        families: &families,
        weight,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    })?;
    let key = format!("system-ui-{variant}-{family}");
    let data = database.with_face_data(id, |bytes, index| {
        let mut data = FontData::from_owned(bytes.to_vec());
        data.index = index;
        data
    })?;
    fonts.font_data.insert(key.clone(), data.into());
    Some(key)
}

#[cfg(target_os = "windows")]
const fn system_ui_font_candidates() -> &'static [&'static str] {
    &["Segoe UI", "Microsoft YaHei UI"]
}

#[cfg(target_os = "macos")]
const fn system_ui_font_candidates() -> &'static [&'static str] {
    &[".AppleSystemUIFont", "PingFang SC", "Helvetica Neue"]
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const fn system_ui_font_candidates() -> &'static [&'static str] {
    &["Noto Sans", "Noto Sans CJK SC", "DejaVu Sans"]
}

// Rebuild egui visuals from a palette. Startup applies the light palette;
// a saved theme change re-applies the matching palette and repaints.
pub(crate) fn apply_visuals(ctx: &egui::Context, palette: &Palette) {
    let mut visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = palette.background;
    visuals.window_fill = palette.surface;
    visuals.window_stroke = Stroke::new(1.0, palette.border);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.hyperlink_color = palette.accent;
    visuals.text_edit_bg_color = Some(palette.surface);
    visuals.widgets.inactive.bg_fill = palette.surface_muted;
    visuals.widgets.inactive.weak_bg_fill = palette.surface_muted;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
    visuals.widgets.hovered.bg_fill = palette.hovered_fill;
    visuals.widgets.hovered.weak_bg_fill = palette.hovered_weak_fill;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.hovered_stroke);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.bg_fill = palette.active_fill;
    visuals.widgets.active.weak_bg_fill = palette.active_weak_fill;
    // Interaction outlines are painted inside the widget rectangle. Keep them
    // exactly one logical pixel so 100% Windows DPI never rasterizes a half-pixel
    // edge into the dark, fuzzy fringe visible around open selectors.
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    visuals.widgets.open.bg_fill = palette.open_fill;
    visuals.widgets.open.weak_bg_fill = palette.open_fill;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, palette.accent);
    visuals.widgets.open.corner_radius = CornerRadius::same(6);
    visuals.text_cursor.stroke = Stroke::new(1.5, palette.accent);
    visuals.selection.bg_fill = palette.selection_fill;
    // TextEdit focus rings reuse `selection.stroke`; using the text color here
    // creates an unintended black outline next to otherwise themed controls.
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    ctx.set_visuals(visuals);
}

pub(crate) const fn icon(icon: Icon) -> IconWidget {
    IconWidget::new(icon)
}

/// Show a compact notification that follows its text up to a shared maximum width.
/// Longer messages wrap instead of stretching the notification across the window.
pub(crate) fn show_toast(
    ctx: &egui::Context,
    id: &'static str,
    message: &str,
    kind: ToastKind,
    anchor_offset: Vec2,
    dismissible: bool,
) -> bool {
    let palette = palette();
    let (icon_kind, fill, border, foreground) = match kind {
        ToastKind::Success => (
            Some(Icon::CheckCircle),
            palette.accent_soft,
            palette.accent_border,
            palette.accent,
        ),
        ToastKind::Error => (
            Some(Icon::AlertCircle),
            palette.error_fill,
            palette.error_stroke,
            palette.error_text,
        ),
        ToastKind::Loading => (
            None,
            palette.accent_soft,
            palette.accent_border,
            palette.accent,
        ),
    };
    let font_size = scaled_font_size(13.0);
    let text_width = ctx.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(
                message.to_owned(),
                egui::FontId::proportional(font_size),
                foreground,
            )
            .size()
            .x
    });
    let close_width = if dismissible {
        TOAST_ITEM_SPACING + TOAST_CLOSE_SIZE
    } else {
        0.0
    };
    let fixed_content_width = TOAST_ICON_SIZE + TOAST_ITEM_SPACING + close_width;
    let available_width = (ctx.content_rect().width() - TOAST_SCREEN_MARGIN).max(1.0);
    let max_width = TOAST_MAX_WIDTH.min(available_width);
    let min_width = TOAST_MIN_WIDTH.min(max_width);
    let width =
        (TOAST_HORIZONTAL_PADDING + fixed_content_width + text_width).clamp(min_width, max_width);
    let label_width = (width - TOAST_HORIZONTAL_PADDING - fixed_content_width).max(1.0);
    let mut dismissed = false;

    egui::Area::new(id.into())
        .order(egui::Order::Tooltip)
        .anchor(Align2::RIGHT_TOP, anchor_offset)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(fill)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(14, 11))
                .show(ui, |ui| {
                    ui.set_width((width - TOAST_HORIZONTAL_PADDING).max(1.0));
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = TOAST_ITEM_SPACING;
                        if let Some(icon_kind) = icon_kind {
                            ui.add(icon(icon_kind).size(TOAST_ICON_SIZE).color(foreground));
                        } else {
                            ui.add(egui::Spinner::new().size(TOAST_ICON_SIZE).color(foreground));
                        }
                        ui.vertical(|ui| {
                            ui.set_width(label_width);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(message).size(font_size).color(foreground),
                                )
                                .wrap(),
                            );
                        });
                        if dismissible && small_icon_button(ui, Icon::X).clicked() {
                            dismissed = true;
                        }
                    });
                });
        });

    dismissed
}

/// A compact icon action painted as one borderless rounded layer.
///
/// Avoiding egui's native button frame prevents its global stroke from leaving
/// anti-aliased corner fragments around transparent and selected icon buttons.
pub(crate) fn icon_button(ui: &mut Ui, glyph: Icon) -> Response {
    painted_icon_button(ui, glyph, false)
}

pub(crate) fn small_icon_button(ui: &mut Ui, glyph: Icon) -> Response {
    painted_icon_button_sized(ui, glyph, false, 28.0, 15.0)
}

/// Icon action used as a tab. The selected state uses a quiet accent surface
/// instead of the high-contrast fill intended for primary actions.
pub(crate) fn selectable_icon_button(ui: &mut Ui, glyph: Icon, selected: bool) -> Response {
    painted_icon_button(ui, glyph, selected)
}

fn painted_icon_button(ui: &mut Ui, glyph: Icon, selected: bool) -> Response {
    painted_icon_button_sized(ui, glyph, selected, 32.0, 17.0)
}

fn painted_icon_button_sized(
    ui: &mut Ui,
    glyph: Icon,
    selected: bool,
    button_size: f32,
    icon_size: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(button_size), Sense::click());
    let palette = palette();
    let fill = if selected {
        palette.accent_soft
    } else if response.is_pointer_button_down_on() {
        ui.visuals().widgets.active.weak_bg_fill
    } else if response.hovered() {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let foreground = if selected {
        palette.accent
    } else {
        palette.text
    };
    let label = glyph.name();

    if ui.is_rect_visible(rect) {
        if fill != Color32::TRANSPARENT {
            paint_compact_rounded_background(ui, rect, 6.0, fill);
        }
        paint_icon(
            ui,
            Rect::from_center_size(rect.center(), Vec2::splat(icon_size)),
            glyph,
            foreground,
        );
    }
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Keep the antialiasing fringe of a compact rounded control inside its
/// allocated rectangle. `epaint` feathers half of the configured width on
/// either side of the path, so the path is inset by that same half-width after
/// the outer bounds have been snapped to physical pixels.
fn paint_compact_rounded_background(ui: &Ui, rect: Rect, radius: f32, fill: Color32) {
    let pixels_per_point = ui.ctx().pixels_per_point();
    let feathering_in_pixels = ui.ctx().tessellation_options(|options| {
        if options.feathering {
            options.feathering_size_in_pixels
        } else {
            0.0
        }
    });
    let (paint_rect, paint_radius) =
        contained_feather_geometry(rect, radius, pixels_per_point, feathering_in_pixels);

    // The geometry is already aligned deliberately. Letting the tessellator
    // round it again would discard the half-pixel feather inset.
    ui.painter().add(
        egui::epaint::RectShape::filled(paint_rect, paint_radius, fill).with_round_to_pixels(false),
    );
}

fn contained_feather_geometry(
    rect: Rect,
    radius: f32,
    pixels_per_point: f32,
    feathering_in_pixels: f32,
) -> (Rect, f32) {
    let pixels_per_point = pixels_per_point.max(f32::EPSILON);
    let aligned_rect = rect.round_to_pixels(pixels_per_point);
    let inset = 0.5 * feathering_in_pixels.max(0.0) / pixels_per_point;
    (aligned_rect.shrink(inset), (radius - inset).max(0.0))
}

/// Full-width navigation/menu row with left-aligned icon and label.
pub(crate) fn navigation_button(ui: &mut Ui, glyph: Icon, label: &str, selected: bool) -> Response {
    let desired_size = Vec2::new(ui.available_width(), 36.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    let palette = palette();
    let fill = if selected {
        palette.accent_soft
    } else if response.is_pointer_button_down_on() {
        ui.visuals().widgets.active.bg_fill
    } else if response.hovered() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let foreground = if selected {
        palette.accent
    } else {
        palette.text
    };

    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 6.0, fill);
        paint_icon(
            ui,
            Rect::from_min_size(
                egui::pos2(rect.left() + 10.0, rect.center().y - 8.5),
                Vec2::splat(17.0),
            ),
            glyph,
            foreground,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 38.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            TextStyle::Body.resolve(ui.style()),
            foreground,
        );
    }
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    response
}

/// Full-width navigation/menu row with a left-aligned label and no icon slot.
pub(crate) fn navigation_text_button(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let desired_size = Vec2::new(ui.available_width(), 36.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    let palette = palette();
    let fill = if selected {
        palette.accent_soft
    } else if response.is_pointer_button_down_on() {
        ui.visuals().widgets.active.bg_fill
    } else if response.hovered() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let foreground = if selected {
        palette.accent
    } else {
        palette.text
    };

    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 6.0, fill);
        ui.painter().text(
            egui::pos2(rect.left() + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            TextStyle::Body.resolve(ui.style()),
            foreground,
        );
    }
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    response
}

/// Text action used in modal footers. Primary and secondary actions share the
/// same dimensions and corner treatment used by the Settings dialog.
pub(crate) fn dialog_action_button(ui: &mut Ui, label: &str, primary: bool) -> Response {
    let palette = palette();
    let button = egui::Button::new(RichText::new(label).color(if primary {
        Color32::WHITE
    } else {
        palette.text
    }))
    .min_size(Vec2::new(68.0, 32.0))
    .corner_radius(6);
    let button = if primary {
        button.fill(palette.accent).stroke(Stroke::NONE)
    } else {
        button.frame_when_inactive(false)
    };
    ui.add(button)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Destructive action used in modal footers. It deliberately shares the same
/// geometry as the regular dialog actions while using the theme's error color.
pub(crate) fn dialog_danger_button(ui: &mut Ui, label: &str) -> Response {
    let button = egui::Button::new(RichText::new(label).color(Color32::WHITE))
        .min_size(Vec2::new(68.0, 32.0))
        .corner_radius(6)
        .fill(palette().error)
        .stroke(Stroke::NONE);
    ui.add(button)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(crate) fn decode_color_image(bytes: &[u8]) -> Result<ColorImage, image::ImageError> {
    let image = image::load_from_memory(bytes)?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_themes_select_the_matching_egui_theme() {
        assert_eq!(
            egui_theme_preference(AppTheme::System),
            egui::ThemePreference::System
        );
        assert_eq!(
            egui_theme_preference(AppTheme::Light),
            egui::ThemePreference::Light
        );
        assert_eq!(
            egui_theme_preference(AppTheme::Dark),
            egui::ThemePreference::Dark
        );
    }

    #[test]
    fn egui_emits_a_native_theme_command_when_the_theme_changes() {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.sync_window_theme = true);
        ctx.set_theme(egui::Theme::Dark);

        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        let commands = &output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("root viewport output")
            .commands;
        assert!(commands.iter().any(|command| matches!(
            command,
            egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark)
        )));
        output.textures_delta.clear();
    }

    #[test]
    fn system_theme_preference_tracks_runtime_system_theme_changes() {
        let ctx = egui::Context::default();
        let mut dark_input = egui::RawInput::default();
        dark_input.system_theme = Some(egui::Theme::Dark);
        let mut output = ctx.run_ui(dark_input, |_| {});
        output.textures_delta.clear();

        set_theme(&ctx, AppTheme::System);
        assert_eq!(theme_preference(), AppTheme::System);
        assert_eq!(theme(), egui::Theme::Dark);

        let mut light_input = egui::RawInput::default();
        light_input.system_theme = Some(egui::Theme::Light);
        let mut output = ctx.run_ui(light_input, |_| {});
        output.textures_delta.clear();
        assert!(sync_system_theme(&ctx, AppTheme::System));
        assert_eq!(theme(), egui::Theme::Light);
    }

    #[test]
    fn interface_line_spacing_scales_with_the_configured_font() {
        assert!(
            (interface_extra_text_line_spacing(EGUI_BASE_FONT_SIZE)
                - EGUI_BASE_EXTRA_TEXT_LINE_SPACING)
                .abs()
                < f32::EPSILON
        );
        assert!(
            interface_extra_text_line_spacing(24.0)
                > interface_extra_text_line_spacing(EGUI_BASE_FONT_SIZE)
        );
    }

    #[test]
    fn active_open_and_focus_outlines_are_crisp_theme_strokes() {
        for palette in [Palette::light(), Palette::dark()] {
            let ctx = egui::Context::default();
            apply_visuals(&ctx, &palette);
            let style = ctx.style_of(ctx.theme());
            let visuals = &style.visuals;

            for stroke in [
                visuals.widgets.active.bg_stroke,
                visuals.widgets.open.bg_stroke,
                visuals.selection.stroke,
            ] {
                assert!((stroke.width - 1.0).abs() < f32::EPSILON);
                assert_eq!(stroke.color, palette.accent);
            }
        }
    }

    #[test]
    fn rounded_controls_keep_pixel_snapping_and_antialiasing() {
        let ctx = egui::Context::default();
        configure_tessellation(&ctx);
        ctx.tessellation_options(|options| {
            assert!(options.feathering);
            assert!((options.feathering_size_in_pixels - 1.0).abs() < f32::EPSILON);
            assert!(options.round_rects_to_pixels);
        });
    }

    #[test]
    fn compact_rounding_contains_the_feathering_fringe() {
        let rect = Rect::from_min_max(egui::pos2(0.2, 0.4), egui::pos2(32.1, 32.2));
        let pixels_per_point = 1.25;
        let feathering_in_pixels = 1.0;
        let aligned_rect = rect.round_to_pixels(pixels_per_point);
        let inset = 0.5 * feathering_in_pixels / pixels_per_point;

        let (paint_rect, paint_radius) =
            contained_feather_geometry(rect, 6.0, pixels_per_point, feathering_in_pixels);

        let restored_outer_rect = paint_rect.expand(inset);
        for (actual, expected) in [
            (restored_outer_rect.min.x, aligned_rect.min.x),
            (restored_outer_rect.min.y, aligned_rect.min.y),
            (restored_outer_rect.max.x, aligned_rect.max.x),
            (restored_outer_rect.max.y, aligned_rect.max.y),
            (paint_radius + inset, 6.0),
        ] {
            assert!((actual - expected).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn dialog_actions_share_the_same_minimum_geometry() {
        let ctx = egui::Context::default();
        let mut primary = Rect::NOTHING;
        let mut secondary = Rect::NOTHING;
        let mut danger = Rect::NOTHING;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            primary = dialog_action_button(ui, "Update", true).rect;
            secondary = dialog_action_button(ui, "Later", false).rect;
            danger = dialog_danger_button(ui, "Remove").rect;
        })
        .drop_without_applying_deltas();

        assert_eq!(primary.size(), Vec2::new(68.0, 32.0));
        assert_eq!(secondary.size(), Vec2::new(68.0, 32.0));
        assert_eq!(danger.size(), Vec2::new(68.0, 32.0));
    }
}
