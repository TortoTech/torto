//! Keep floating initials distinct from ordinary local font sizes and floats.
use super::*;
use rebook_publication::DropCap;
use unicode_segmentation::UnicodeSegmentation;

fn initial_length(text: &str) -> Option<usize> {
    for (start, letter) in text.grapheme_indices(true) {
        let first = letter.chars().next()?;
        if first.is_alphabetic() {
            let mut end = start + letter.len();
            end += text[end..]
                .chars()
                .take_while(|c| matches!(c, '\'' | '"' | '’' | '”' | '»' | '」' | '』'))
                .map(char::len_utf8)
                .sum::<usize>();
            return Some(end);
        }
        if !matches!(first, '\'' | '"' | '‘' | '“' | '«' | '「' | '『') {
            break;
        }
    }
    None
}

fn float_side(properties: &HashMap<String, String>) -> Option<bool> {
    match properties.get("float").map(String::as_str) {
        Some("left") => Some(false),
        Some("right") => Some(true),
        _ => None,
    }
}

fn geometry(
    properties: &HashMap<String, String>,
    scale: f32,
    root_scale: f32,
    inherited_line_height: f32,
    ordinary_scale: f32,
) -> Option<DropCap> {
    let cap_length = |value: &str| {
        let (number, unit) = if let Some(value) = value.strip_suffix("rem") {
            (value, root_scale)
        } else if let Some(value) = value.strip_suffix("em") {
            (value, scale)
        } else if let Some(value) = value.strip_suffix("px") {
            (value, 1.0 / 16.0)
        } else if let Some(value) = value.strip_suffix("pt") {
            (value, 1.0 / 12.0)
        } else {
            (value, 0.0)
        };
        let number = number.parse::<f32>().ok()?;
        (number.is_finite() && (unit != 0.0 || number == 0.0))
            .then_some((number * unit).clamp(-8.0, 8.0))
    };
    let sides = |prefix: &str| {
        ["top", "right", "bottom", "left"].map(|side| {
            properties
                .get(&format!("{prefix}-{side}"))
                .and_then(|value| cap_length(value))
                .unwrap_or(0.0)
        })
    };
    let line_height = properties
        .get("line-height")
        .and_then(|value| {
            if let Some(percent) = value.strip_suffix('%') {
                percent.parse::<f32>().ok().map(|n| n / 100.0)
            } else {
                value
                    .parse::<f32>()
                    .ok()
                    .or_else(|| cap_length(value).map(|n| n / scale))
            }
        })
        .filter(|n| (0.0..=5.0).contains(n))
        .unwrap_or(inherited_line_height);
    let margin = sides("margin");
    let padding = sides("padding");
    Some(DropCap {
        right: float_side(properties)?,
        insets: std::array::from_fn(|i| margin[i] + padding[i].max(0.0)),
        line_height,
        ordinary_scale,
    })
}

impl StyleSheet {
    pub(super) fn mark_floating_initial(
        &self,
        node: Node<'_, '_>,
        style: &mut TextStyle,
        ordinary_scale: f32,
    ) {
        // Only a standalone initial at the head of a paragraph, not a floating
        // label, icon, equation number, superscript, or an entire word.
        if !node.parent_element().is_some_and(|p| p.has_tag_name("p"))
            || style.baseline != rebook_publication::TextBaseline::Normal
            || style.inline_size_scale.unwrap_or(1.0) < 1.25
            || node.children().any(|n| n.is_element())
        {
            return;
        }
        let properties = self.cascaded_properties(node);
        let text = node.text().unwrap_or_default().trim();
        if float_side(&properties).is_some() && initial_length(text) == Some(text.len()) {
            style.drop_cap = geometry(
                &properties,
                style.size_scale,
                self.root_font_size.unwrap_or(1.0),
                self.block_style(node, BlockStyle::default()).line_height,
                ordinary_scale,
            );
        }
    }

    pub(super) fn apply_first_letter(&self, node: Node<'_, '_>, content: &mut Vec<Inline>) {
        if self.first_letter_rules.is_empty() {
            return;
        }
        let Some(Inline::Text(first)) = content.first_mut() else {
            return;
        };
        if first.style.drop_cap.is_some()
            || first.link.is_some()
            || first.style.baseline != rebook_publication::TextBaseline::Normal
            || first.style.inline_role != InlineRole::Normal
        {
            return;
        }
        let rules = self
            .first_letter_index
            .matching(node, &self.first_letter_rules);
        if rules.is_empty() {
            return;
        }
        let mut properties = HashMap::new();
        for rule in &rules {
            insert_declarations(&mut properties, rule.declarations.iter().cloned());
        }
        for rule in rules {
            insert_declarations(&mut properties, rule.important_declarations.iter().cloned());
        }
        if float_side(&properties).is_none() {
            return;
        }
        let Some(end) = initial_length(&first.text) else {
            return;
        };
        let mut style = first.style;
        let root = self.root_font_size.unwrap_or(1.0);
        apply_text_properties(&mut style, &properties, first.style.size_scale, root);
        style.drop_cap = geometry(
            &properties,
            style.size_scale,
            root,
            self.block_style(node, BlockStyle::default()).line_height,
            first.style.size_scale,
        );
        let initial = Inline::Text(TextRun {
            text: first.text[..end].to_owned(),
            style,
            link: None,
        });
        if end == first.text.len() {
            content[0] = initial;
        } else {
            first.text.drain(..end);
            content.insert(0, initial);
        }
    }
}
