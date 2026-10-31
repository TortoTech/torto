use ratex_layout::{LayoutOptions, layout, to_display_list};
use ratex_svg::{SvgOptions, render_to_svg};
use ratex_types::{color::Color, math_style::MathStyle};

/// Formula geometry and SVG content in Torto's baseline-oriented coordinate system.
pub struct MathResult {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    /// SVG nodes whose baseline is at `y = 0` and whose top is at `-ascent`.
    pub svg_fragment: String,
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "formula dimensions are bounded UI values converted from RaTeX f64 units"
)]
pub fn render_math(
    latex: &str,
    font_size: f32,
    text_color: &str,
    display: bool,
) -> Result<MathResult, String> {
    if !font_size.is_finite() || font_size <= 0.0 {
        return Err(format!("invalid formula font size: {font_size}"));
    }

    let normalized = normalize_ocr_latex(latex);
    let nodes =
        ratex_parser::parse(&normalized).map_err(|error| format!("LaTeX parse error: {error}"))?;
    let color = Color::from_hex(text_color)
        .ok_or_else(|| format!("unsupported formula color: {text_color}"))?;
    let options = LayoutOptions {
        style: if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        color,
        ..LayoutOptions::default()
    };
    let root = layout(&nodes, &options);
    let display_list = to_display_list(&root);
    if !display_list.width.is_finite()
        || !display_list.height.is_finite()
        || !display_list.depth.is_finite()
    {
        return Err("formula layout produced non-finite geometry".to_owned());
    }

    let scale = f64::from(font_size);
    let width = (display_list.width * scale) as f32;
    let ascent = (display_list.height * scale) as f32;
    let descent = (display_list.depth * scale) as f32;
    let document = render_to_svg(
        &display_list,
        &SvgOptions {
            font_size: scale,
            padding: 0.0,
            stroke_width: scale * 0.0375,
            embed_glyphs: true,
            ..SvgOptions::default()
        },
    );
    let body = svg_body(&document)?;
    let svg_fragment = format!(r#"<g transform="translate(0,-{ascent:.4})">{body}</g>"#);

    Ok(MathResult {
        width,
        ascent,
        descent,
        svg_fragment,
    })
}

fn svg_body(document: &str) -> Result<&str, String> {
    let start = document
        .find('>')
        .map(|index| index + 1)
        .ok_or_else(|| "RaTeX returned malformed SVG without a root start tag".to_owned())?;
    let end = document
        .rfind("</svg>")
        .ok_or_else(|| "RaTeX returned malformed SVG without a root end tag".to_owned())?;
    if start > end {
        return Err("RaTeX returned malformed SVG root bounds".to_owned());
    }
    Ok(&document[start..end])
}

fn normalize_ocr_latex(latex: &str) -> String {
    let normalized = rewrite_braced_command(latex, r"\uwave", |content| {
        format!(r"\underline{{{content}}}")
    });
    repair_fraction_denominator_scripts(&normalized)
}

fn rewrite_braced_command(
    latex: &str,
    command: &str,
    mut replacement: impl FnMut(&str) -> String,
) -> String {
    let mut output = String::with_capacity(latex.len());
    let mut cursor = 0;
    while let Some(relative_start) = latex[cursor..].find(command) {
        let start = cursor + relative_start;
        let after_command = start + command.len();
        if is_escaped_latex_command(latex, start)
            || latex[after_command..]
                .chars()
                .next()
                .is_some_and(char::is_alphabetic)
        {
            output.push_str(&latex[cursor..after_command]);
            cursor = after_command;
            continue;
        }

        let group_start = skip_whitespace(latex, after_command);
        if !latex[group_start..].starts_with('{') {
            output.push_str(&latex[cursor..after_command]);
            cursor = after_command;
            continue;
        }
        let Some(group_end) = find_balanced_group_end(latex, group_start) else {
            output.push_str(&latex[cursor..]);
            return output;
        };

        output.push_str(&latex[cursor..start]);
        output.push_str(&replacement(&latex[group_start + 1..group_end]));
        cursor = group_end + 1;
    }
    output.push_str(&latex[cursor..]);
    output
}

fn repair_fraction_denominator_scripts(latex: &str) -> String {
    let mut normalized = latex.to_owned();
    while let Some(repaired) = repair_one_fraction_denominator_script(&normalized) {
        normalized = repaired;
    }
    normalized
}

fn repair_one_fraction_denominator_script(latex: &str) -> Option<String> {
    const COMMAND: &str = r"\frac";
    let mut cursor = 0;
    while let Some(relative_start) = latex[cursor..].find(COMMAND) {
        let start = cursor + relative_start;
        let after_command = start + COMMAND.len();
        if is_escaped_latex_command(latex, start)
            || latex[after_command..]
                .chars()
                .next()
                .is_some_and(char::is_alphabetic)
        {
            cursor = after_command;
            continue;
        }

        let numerator_start = skip_whitespace(latex, after_command);
        if !latex[numerator_start..].starts_with('{') {
            cursor = after_command;
            continue;
        }
        let numerator_end = find_balanced_group_end(latex, numerator_start)?;
        let script_start = skip_whitespace(latex, numerator_end + 1);
        if !latex[script_start..].starts_with('_') {
            cursor = after_command;
            continue;
        }
        let denominator_start = skip_whitespace(latex, script_start + 1);
        if !latex[denominator_start..].starts_with('{')
            || find_balanced_group_end(latex, denominator_start).is_none()
        {
            cursor = after_command;
            continue;
        }

        let mut repaired = String::with_capacity(latex.len() - 1);
        repaired.push_str(&latex[..script_start]);
        repaired.push_str(&latex[script_start + 1..]);
        return Some(repaired);
    }
    None
}

fn skip_whitespace(latex: &str, mut cursor: usize) -> usize {
    while latex[cursor..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
    {
        cursor += latex[cursor..].chars().next().unwrap().len_utf8();
    }
    cursor
}

fn find_balanced_group_end(latex: &str, start: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut escaped = false;
    for (offset, character) in latex[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_escaped_latex_command(latex: &str, command_start: usize) -> bool {
    latex[..command_start]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
}

#[cfg(test)]
mod tests {
    use super::{normalize_ocr_latex, render_math, svg_body};

    #[test]
    fn basic_formula_uses_baseline_oriented_svg_geometry() {
        let rendered = render_math(r"E=mc^2", 16.0, "#262624", false).unwrap();

        assert!(rendered.width > 0.0);
        assert!(rendered.ascent > 0.0);
        assert!(rendered.descent >= 0.0);
        assert!(rendered.svg_fragment.starts_with("<g transform="));
        assert!(!rendered.svg_fragment.contains("<svg"));
        assert!(!rendered.svg_fragment.contains("PARSE ERROR"));
    }

    #[test]
    fn fraction_glyphs_are_embedded_without_system_fonts() {
        let rendered = render_math(r"\frac{1}{2}", 72.0, "#262624", true).unwrap();
        assert!(
            !rendered.svg_fragment.contains("<text"),
            "{}",
            rendered.svg_fragment
        );
        assert!(
            rendered.svg_fragment.matches("<path").count() >= 2,
            "{}",
            rendered.svg_fragment
        );
    }

    #[test]
    fn common_paddleocr_formulas_render_through_ratex() {
        let samples = [
            r"^{2}",
            r"C^{^{\prime}}",
            r"\varepsilon_{_{0}}",
            r"\begin{array}{rl}a&=b\\c&=d\end{array}",
            r"\begin{aligned}a&=b\\c&=d\end{aligned}",
            r"\mathcal{V}\leftrightarrow V",
            r"A\xrightarrow{蒸发}\boxed{B}\xrightarrow{雨量}C",
            r"\sum\limits_{\substack{x>0\\y>0}}x",
            r"\frac{a}{b}\Bigg|c",
            r"\Pr(A)\quad\mathring{A}\quad x^{\prime}",
            r"1\AA\hspace{1em}\vert x\vert",
            r"\begin{array}{r}a\\\hline b\end{array}",
            r"4\text{千卡/克}",
        ];

        for latex in samples {
            let rendered = render_math(latex, 16.0, "#262624", true)
                .unwrap_or_else(|error| panic!("{latex}: {error}"));
            assert!(rendered.width.is_finite(), "{latex}");
            assert!(!rendered.svg_fragment.contains("PARSE ERROR"), "{latex}");
        }
    }

    #[test]
    fn unsupported_ocr_uwave_has_a_narrow_semantic_fallback() {
        let normalized = normalize_ocr_latex(r"\uwave{\text{波浪线}} + x");

        assert_eq!(normalized, r"\underline{\text{波浪线}} + x");
        assert!(render_math(&normalized, 16.0, "#262624", false).is_ok());
    }

    #[test]
    fn malformed_ocr_fraction_denominator_marker_is_repaired() {
        let latex = r"\theta\sim\frac{\overbrace{Gm/rc}^{v\downarrow}}_{c}";
        let normalized = normalize_ocr_latex(latex);

        assert_eq!(
            normalized,
            r"\theta\sim\frac{\overbrace{Gm/rc}^{v\downarrow}}{c}"
        );
        assert!(render_math(latex, 16.0, "#262624", true).is_ok());
    }

    #[test]
    fn malformed_svg_roots_are_rejected() {
        assert!(svg_body("<svg>body</svg>").is_ok());
        assert!(svg_body("body</svg>").is_err());
        assert!(svg_body("<svg>body").is_err());
    }
}
