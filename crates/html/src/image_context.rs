//! Retain containing-block widths while flattening HTML into reading blocks.
use super::*;
use rebook_publication::ImageContainerWidth;

const COLUMN: ImageContainerWidth = ImageContainerWidth {
    fraction: Some(1.0),
    pixels: None,
};

fn relative_width(parent: ImageContainerWidth, length: ImageLength) -> ImageContainerWidth {
    match length {
        ImageLength::Pixels(pixels) => ImageContainerWidth {
            fraction: None,
            pixels: Some(pixels),
        },
        ImageLength::Fraction(scale) => ImageContainerWidth {
            fraction: parent.fraction.map(|value| (value * scale).min(f32::MAX)),
            pixels: parent.pixels.map(|value| (value * scale).min(f32::MAX)),
        },
    }
}

fn constrain(width: ImageContainerWidth, max: ImageContainerWidth) -> ImageContainerWidth {
    let minimum = |a: Option<f32>, b: Option<f32>| match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    ImageContainerWidth {
        fraction: minimum(width.fraction, max.fraction),
        pixels: minimum(width.pixels, max.pixels),
    }
}

impl ReadingIrParser<'_> {
    pub(super) fn image_container_width(
        &mut self,
        image: Node<'_, '_>,
    ) -> Option<ImageContainerWidth> {
        // Cache by node within this document only. Sibling images share the
        // ancestor cascade; the cache dies with the parser and never retains DOMs.
        let mut pending = Vec::new();
        let mut width = COLUMN;
        for ancestor in image.ancestors().skip(1).filter(Node::is_element) {
            if let Some(cached) = self.image_width_contexts.get(&ancestor.id()) {
                width = *cached;
                break;
            }
            pending.push(ancestor);
        }
        for ancestor in pending.into_iter().rev() {
            let properties = self.styles.cascaded_properties(ancestor);
            let establishes_width = is_block_boundary(ancestor.tag_name().name())
                || matches!(ancestor.tag_name().name(), "html" | "body" | "td" | "th")
                || properties.get("display").is_some_and(|display| {
                    matches!(
                        display.as_str(),
                        "block" | "inline-block" | "table" | "flex" | "grid"
                    )
                });
            if establishes_width {
                let parent = width;
                if let Some(length) = properties
                    .get("width")
                    .map(String::as_str)
                    .or_else(|| attribute_local(ancestor, "width"))
                    .and_then(image_length)
                {
                    width = relative_width(parent, length);
                }
                if let Some(max) = properties
                    .get("max-width")
                    .and_then(|value| image_length(value))
                {
                    width = constrain(width, relative_width(parent, max));
                }
            }
            self.image_width_contexts.insert(ancestor.id(), width);
        }
        (width != COLUMN).then_some(width)
    }
}
