//! Resolve applicable HTML dimension hints and CSS containing blocks separately
//! from overflow clipping. A small containing block does not shrink auto images.
use super::*;
use rebook_publication::ImageContainerWidth;

const COLUMN: ImageContainerWidth = ImageContainerWidth {
    fraction: Some(1.0),
    pixels: None,
};
#[derive(Clone, Copy)]
pub(super) struct ImageContext {
    pub width: ImageContainerWidth,
    pub height: Option<ImageContainerWidth>,
    pub clip_width: Option<ImageContainerWidth>,
    pub clip_height: Option<ImageContainerWidth>,
}
impl Default for ImageContext {
    fn default() -> Self {
        Self {
            width: COLUMN,
            height: None,
            clip_width: None,
            clip_height: None,
        }
    }
}
fn relative_width(parent: ImageContainerWidth, length: ImageLength) -> ImageContainerWidth {
    match length {
        ImageLength::Pixels(pixels) => ImageContainerWidth {
            fraction: None,
            pixels: Some(pixels),
        },
        ImageLength::Fraction(scale) => ImageContainerWidth {
            fraction: parent.fraction.map(|v| (v * scale).min(f32::MAX)),
            pixels: parent.pixels.map(|v| (v * scale).min(f32::MAX)),
        },
    }
}
fn constrain(a: ImageContainerWidth, b: ImageContainerWidth) -> ImageContainerWidth {
    let min = |a: Option<f32>, b: Option<f32>| match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    ImageContainerWidth {
        fraction: min(a.fraction, b.fraction),
        pixels: min(a.pixels, b.pixels),
    }
}
/// HTML's dimension parser is not a CSS length parser. Legacy suffixes do not
/// turn attribute numbers into em/rem; table/cell hints additionally ignore zero.
pub(super) fn html_dimension(value: &str, ignore_zero: bool) -> Option<ImageLength> {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let mut end = digits;
    if value.as_bytes().get(end) == Some(&b'.') {
        end += 1;
        end += value[end..].bytes().take_while(u8::is_ascii_digit).count();
    }
    let n: f32 = value[..end].parse().ok()?;
    if !n.is_finite() || ignore_zero && n == 0.0 {
        return None;
    }
    Some(if value.as_bytes().get(end) == Some(&b'%') {
        ImageLength::Fraction(n / 100.0)
    } else {
        ImageLength::Pixels(n)
    })
}
pub(super) fn hint(node: Node<'_, '_>, axis: &str) -> Option<ImageLength> {
    // Unsupported attributes on p/div/blockquote are still available to CSS
    // attribute selectors; they do not independently create presentation hints.
    if node.tag_name().namespace() == Some("http://www.w3.org/2000/svg") {
        return attribute_local(node, axis)
            .and_then(|v| image_length(v).or_else(|| html_dimension(v, false)));
    }
    match (node.tag_name().name(), axis) {
        ("img", "width" | "height") => {
            attribute_local(node, axis).and_then(|v| html_dimension(v, false))
        }
        ("table" | "td" | "th", "width") | ("td" | "th", "height") => {
            attribute_local(node, axis).and_then(|v| html_dimension(v, true))
        }
        ("table" | "tr" | "thead" | "tbody" | "tfoot", "height") => {
            attribute_local(node, axis).and_then(|v| html_dimension(v, false))
        }
        _ => None,
    }
}
pub(super) fn dimension(
    styles: &StyleSheet,
    node: Node<'_, '_>,
    properties: &HashMap<String, String>,
    axis: &str,
) -> Option<ImageLength> {
    match properties.get(axis).map(String::as_str) {
        Some("inherit") => node
            .ancestors()
            .skip(1)
            .find(Node::is_element)
            .and_then(|parent| {
                dimension(styles, parent, &styles.cascaded_properties(parent), axis)
            }),
        Some("auto" | "initial" | "unset" | "revert" | "revert-layer") => None,
        Some(v) => image_length(v),
        None => hint(node, axis),
    }
}
impl ReadingIrParser<'_> {
    pub(super) fn image_geometry(&mut self, image: Node<'_, '_>) -> ImageContext {
        let mut pending = Vec::new();
        let mut context = ImageContext::default();
        for ancestor in image.ancestors().skip(1).filter(Node::is_element) {
            if let Some(cached) = self.image_width_contexts.get(&ancestor.id()) {
                context = *cached;
                break;
            }
            pending.push(ancestor);
        }
        for ancestor in pending.into_iter().rev() {
            let p = self.styles.cascaded_properties(ancestor);
            let establishes = match p.get("display").map(String::as_str) {
                Some("inline" | "contents" | "none") => false,
                Some(
                    "block" | "inline-block" | "table" | "table-cell" | "flex" | "inline-flex"
                    | "grid" | "inline-grid" | "flow-root" | "list-item",
                ) => true,
                _ => {
                    is_block_boundary(ancestor.tag_name().name())
                        || matches!(ancestor.tag_name().name(), "html" | "body" | "td" | "th")
                }
            };
            if establishes {
                let parent = context;
                if let Some(v) = dimension(&self.styles, ancestor, &p, "width") {
                    context.width = relative_width(parent.width, v);
                }
                if let Some(v) = p.get("max-width").and_then(|v| image_length(v)) {
                    context.width = constrain(context.width, relative_width(parent.width, v));
                }
                let parent_height = if ancestor.has_tag_name("html") {
                    Some(COLUMN)
                } else {
                    parent.height
                };
                context.height =
                    dimension(&self.styles, ancestor, &p, "height").and_then(|v| match v {
                        ImageLength::Pixels(_) => Some(relative_width(COLUMN, v)),
                        ImageLength::Fraction(_) => parent_height.map(|h| relative_width(h, v)),
                    });
                let mut shorthand = p
                    .get("overflow")
                    .map(String::as_str)
                    .unwrap_or("visible")
                    .split_ascii_whitespace();
                let first = shorthand.next().unwrap_or("visible");
                let second = shorthand.next().unwrap_or(first);
                let mut x = p.get("overflow-x").map(String::as_str).unwrap_or(first);
                let mut y = p.get("overflow-y").map(String::as_str).unwrap_or(second);
                let scrolls = |v| matches!(v, "hidden" | "scroll" | "auto");
                if scrolls(x) {
                    y = match y {
                        "visible" => "auto",
                        "clip" => "hidden",
                        _ => y,
                    };
                }
                if scrolls(y) {
                    x = match x {
                        "visible" => "auto",
                        "clip" => "hidden",
                        _ => x,
                    };
                }
                let clips = |axis: &str| {
                    matches!(
                        if axis == "overflow-x" { x } else { y },
                        "hidden" | "clip" | "scroll" | "auto"
                    )
                };
                if clips("overflow-x") {
                    context.clip_width = Some(
                        parent
                            .clip_width
                            .map_or(context.width, |v| constrain(v, context.width)),
                    );
                }
                if clips("overflow-y")
                    && let Some(h) = context.height
                {
                    context.clip_height = Some(parent.clip_height.map_or(h, |v| constrain(v, h)));
                }
            }
            self.image_width_contexts.insert(ancestor.id(), context);
        }
        context
    }
}
