//! Deterministic image fallback before a reader viewport exists. Conditional
//! picture sources are left to their ordinary img fallback; no media evaluation.
use super::attribute_local;
use roxmltree::Node;

pub(super) fn source<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    if let Some(picture) = node.parent().filter(|p| p.has_tag_name("picture")) {
        for source in picture.children().filter(|n| n.has_tag_name("source")) {
            if attribute_local(source, "media").is_some_and(|m| !m.trim().is_empty()) {
                continue;
            }
            if attribute_local(source, "type").is_some_and(|t| {
                !matches!(
                    t.trim(),
                    "image/png"
                        | "image/jpeg"
                        | "image/gif"
                        | "image/webp"
                        | "image/bmp"
                        | "image/svg+xml"
                )
            }) {
                continue;
            }
            if let Some(value) = attribute_local(source, "srcset").and_then(srcset) {
                return Some(value);
            }
        }
    }
    attribute_local(node, "src")
        .or_else(|| attribute_local(node, "href"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| attribute_local(node, "srcset").and_then(srcset))
}

fn srcset(value: &str) -> Option<&str> {
    if value.len() > 16 * 1024 {
        return None;
    }
    let mut rest = value;
    let mut choices = Vec::new();
    while choices.len() < 64 {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if rest.is_empty() {
            break;
        }
        let end = rest
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let token = &rest[..end];
        rest = &rest[end..];
        let (url, descriptor) = if token.ends_with(',') {
            (token.trim_end_matches(','), "")
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            let descriptor = rest[..end].trim();
            rest = &rest[end..];
            (token, descriptor)
        };
        let value = if descriptor.is_empty() {
            Some((1.0, false))
        } else if let Some(density) = descriptor.strip_suffix('x') {
            density.parse::<f32>().ok().map(|x| (x, false))
        } else if let Some(width) = descriptor.strip_suffix('w') {
            width.parse::<u32>().ok().map(|w| (w as f32 / 800.0, true))
        } else {
            None
        };
        if let Some((scale, _)) = value.filter(|(n, _)| n.is_finite() && *n > 0.0)
            && !url.is_empty()
        {
            choices.push((url, scale));
        }
    }
    // Prefer the smallest candidate at/above 1x (or 800px), then the largest
    // smaller candidate. Selection stays stable across pagination and resizing.
    choices
        .iter()
        .filter(|(_, s)| *s >= 1.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .or_else(|| choices.iter().max_by(|a, b| a.1.total_cmp(&b.1)))
        .map(|(url, _)| *url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn srcset_preserves_url_commas_and_chooses_a_bounded_density() {
        assert_eq!(
            srcset("small.png 0.5x, normal.png 1x, large.png 2x"),
            Some("normal.png")
        );
        assert_eq!(
            srcset("small.png 320w, medium.png 800w, big.png 2400w"),
            Some("medium.png")
        );
        assert_eq!(srcset("a,b.png 1x, other.png 2x"), Some("a,b.png"));
        assert_eq!(srcset("broken.png NaNx, other.png 2x"), Some("other.png"));
    }
}
