use egui::{Color32, Rect, Response, Sense, Ui, Vec2, Widget, WidgetInfo, WidgetType};

macro_rules! asset {
    ($name:literal) => {{
        (
            $name,
            include_bytes!(concat!("../../assets/icons/", $name, ".svg")) as &'static [u8],
        )
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    AlertCircle,
    BookOpen,
    Bot,
    BrainCircuit,
    CheckCircle,
    ChevronDown,
    ChevronRight,
    Cloud,
    Copy,
    ExternalLink,
    Highlighter,
    Info,
    Keyboard,
    Languages,
    Library,
    ListTree,
    Maximize2,
    Menu,
    MessageCircle,
    MessageCircleQuestion,
    MessageSquarePlus,
    MessageSquareText,
    Minimize2,
    Minus,
    Moon,
    PanelLeft,
    Pencil,
    Pin,
    PinOff,
    Plus,
    ScanText,
    Search,
    Send,
    Server,
    Settings,
    Sun,
    Trash2,
    Type,
    X,
}

impl Icon {
    #[cfg(test)]
    const ALL: [Self; 39] = [
        Self::AlertCircle,
        Self::BookOpen,
        Self::Bot,
        Self::BrainCircuit,
        Self::CheckCircle,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::Cloud,
        Self::Copy,
        Self::ExternalLink,
        Self::Highlighter,
        Self::Info,
        Self::Keyboard,
        Self::Languages,
        Self::Library,
        Self::ListTree,
        Self::Maximize2,
        Self::Menu,
        Self::MessageCircle,
        Self::MessageCircleQuestion,
        Self::MessageSquarePlus,
        Self::MessageSquareText,
        Self::Minimize2,
        Self::Minus,
        Self::Moon,
        Self::PanelLeft,
        Self::Pencil,
        Self::Pin,
        Self::PinOff,
        Self::Plus,
        Self::ScanText,
        Self::Search,
        Self::Send,
        Self::Server,
        Self::Settings,
        Self::Sun,
        Self::Trash2,
        Self::Type,
        Self::X,
    ];

    fn asset(self) -> (&'static str, &'static [u8]) {
        match self {
            Self::AlertCircle => asset!("alert-circle"),
            Self::BookOpen => asset!("book-open"),
            Self::Bot => asset!("bot"),
            Self::BrainCircuit => asset!("brain-circuit"),
            Self::CheckCircle => asset!("check-circle"),
            Self::ChevronDown => asset!("chevron-down"),
            Self::ChevronRight => asset!("chevron-right"),
            Self::Cloud => asset!("cloud"),
            Self::Copy => asset!("copy"),
            Self::ExternalLink => asset!("external-link"),
            Self::Highlighter => asset!("highlighter"),
            Self::Info => asset!("info"),
            Self::Keyboard => asset!("keyboard"),
            Self::Languages => asset!("languages"),
            Self::Library => asset!("library"),
            Self::ListTree => asset!("list-tree"),
            Self::Maximize2 => asset!("maximize-2"),
            Self::Menu => asset!("menu"),
            Self::MessageCircle => asset!("message-circle"),
            Self::MessageCircleQuestion => asset!("message-circle-question"),
            Self::MessageSquarePlus => asset!("message-square-plus"),
            Self::MessageSquareText => asset!("message-square-text"),
            Self::Minimize2 => asset!("minimize-2"),
            Self::Minus => asset!("minus"),
            Self::Moon => asset!("moon"),
            Self::PanelLeft => asset!("panel-left"),
            Self::Pencil => asset!("pencil"),
            Self::Pin => asset!("pin"),
            Self::PinOff => asset!("pin-off"),
            Self::Plus => asset!("plus"),
            Self::ScanText => asset!("scan-text"),
            Self::Search => asset!("search"),
            Self::Send => asset!("send"),
            Self::Server => asset!("server"),
            Self::Settings => asset!("settings"),
            Self::Sun => asset!("sun"),
            Self::Trash2 => asset!("trash-2"),
            Self::Type => asset!("type"),
            Self::X => asset!("x"),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        self.asset().0
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IconWidget {
    glyph: Icon,
    size: f32,
    color: Option<Color32>,
}

impl IconWidget {
    pub(crate) const fn new(glyph: Icon) -> Self {
        Self {
            glyph,
            size: 17.0,
            color: None,
        }
    }

    pub(crate) const fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub(crate) const fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }
}

impl Widget for IconWidget {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::hover());
        if ui.is_rect_visible(rect) {
            paint_icon(
                ui,
                rect,
                self.glyph,
                self.color.unwrap_or_else(|| ui.visuals().text_color()),
            );
        }
        response.widget_info(|| {
            WidgetInfo::labeled(WidgetType::Image, ui.is_enabled(), self.glyph.name())
        });
        response
    }
}

pub(crate) fn paint_icon(ui: &Ui, rect: Rect, glyph: Icon, color: Color32) {
    let (name, bytes) = glyph.asset();
    egui::Image::from_bytes(format!("bytes://lucide/{name}.svg"), bytes)
        .fit_to_exact_size(rect.size())
        .texture_options(egui::TextureOptions::LINEAR)
        .tint(color)
        .show_loading_spinner(false)
        .paint_at(ui, rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_selected_lucide_assets_are_valid_tintable_svgs() {
        let options = resvg::usvg::Options::default();
        for glyph in Icon::ALL {
            let (name, bytes) = glyph.asset();
            assert!(
                !bytes
                    .windows(b"currentColor".len())
                    .any(|value| value == b"currentColor"),
                "{name} must use a tintable concrete source color"
            );
            resvg::usvg::Tree::from_data(bytes, &options)
                .unwrap_or_else(|error| panic!("invalid {name}.svg: {error}"));
        }
    }
}
