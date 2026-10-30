//! Formula and aligned symbol definitions share one raster and one transform.
use super::*;
use rebook_publication::{FormulaNote, RasterResource};

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn lines(text: &str, width: f32, size: f32) -> Vec<String> {
    let advance = |ch: char| {
        if ch.is_whitespace() {
            size * 0.5
        } else if ch.is_ascii_lowercase() && !matches!(ch, 'm' | 'w') || ch.is_ascii_digit() {
            size * 0.75
        } else {
            size
        }
    };
    let mut result = Vec::new();
    let mut line = String::new();
    let mut used = 0.0;
    for ch in text.chars() {
        // A conservative em bound also covers CJK fallback fonts. Explicit
        // newlines and narrow viewports must never let a description cross columns.
        if ch == '\n' {
            result.push(std::mem::take(&mut line));
            used = 0.0;
            continue;
        }
        if used + advance(ch) > width && !line.is_empty() {
            // Keep Latin words together; exceptionally long words still wrap.
            if let Some(at) = line.rfind(char::is_whitespace).filter(|at| *at > 0) {
                let tail = line[at..].trim_start().to_owned();
                line.truncate(at);
                result.push(std::mem::replace(&mut line, tail));
                used = line.chars().map(&advance).sum();
            } else {
                result.push(std::mem::take(&mut line));
                used = 0.0;
            }
        }
        if !line.is_empty() || !ch.is_whitespace() {
            line.push(ch);
            used += advance(ch);
        }
    }
    result.push(line);
    result
}

/// No filesystem or font discovery: geometry can be prepared on a layout worker.
fn geometry(
    notes: &[FormulaNote],
    width: f32,
    size: f32,
    color: Rgba,
) -> Result<(String, f32), String> {
    let ink = format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue);
    let padding = (size * 0.5).min(width * 0.03);
    let gap = (size * 0.65).min(width * 0.04);
    let symbols = notes
        .iter()
        .take(32)
        .map(|note| rebook_math::math::render_math(&note.symbol, size, &ink, false))
        .collect::<Result<Vec<_>, _>>()?;
    let symbol_width = symbols
        .iter()
        .map(|symbol| symbol.width + 4.0)
        .fold(4.0_f32, f32::max)
        .min(width * 0.35)
        .max(4.0);
    let description_x = padding + symbol_width + gap;
    let description_width = (width - description_x - padding).max(size);
    let line_height = size * 1.45;
    let mut y = padding;
    let mut body = String::new();
    for (note, symbol) in notes.iter().zip(symbols) {
        let scale = ((symbol_width - 4.0).max(1.0) / symbol.width.max(1.0)).min(1.0);
        let symbol_height = (symbol.ascent + symbol.descent) * scale;
        body.push_str(&format!(
            r#"<g transform="translate({padding} {}) scale({scale})">{}</g>"#,
            y + symbol.ascent * scale + 2.0,
            symbol.svg_fragment
        ));
        let description = if note.unit.is_empty() {
            note.meaning.clone()
        } else {
            format!("{} ({})", note.meaning, note.unit)
        };
        let rows = lines(&description, description_width, size);
        for (index, line) in rows.iter().enumerate() {
            body.push_str(&format!(r#"<text x="{description_x}" y="{}" font-family="sans-serif" font-size="{size}" fill="{ink}" text-anchor="start">{}</text>"#,
                y + size + index as f32 * line_height, escape(line)));
        }
        y += symbol_height.max(rows.len() as f32 * line_height) + size * 0.5;
    }
    let height = (y + padding).max(1.0);
    Ok((
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}">{body}</svg>"#
        ),
        height,
    ))
}

pub(super) fn append(
    body: (RasterImage, f32, f32),
    notes: &[FormulaNote],
    width: f32,
    typography: &ReaderTypography,
    color: Rgba,
    options: &resvg::usvg::Options<'_>,
    deferred: Option<(&dyn BookSource, u64)>,
) -> Result<(RasterImage, f32, f32), String> {
    if notes.is_empty() {
        return Ok(body);
    }
    let size = (typography.font_size * 0.85).max(typography.minimum_font_size);
    let width = width.max(1.0);
    let (svg, height) = geometry(notes, width, size, color)?;
    let notes_image = formula_raster::prepare_svg(svg, width, height, options, deferred)?;
    let (body, bw, bh) = body;
    let gap = size * 0.6;
    let total_height = bh + gap + height;
    let image = formula_raster::compose(
        width,
        total_height,
        vec![
            formula_raster::Placement {
                image: body,
                x: (width - bw) * 0.5,
                y: 0.0,
                width: bw,
                height: bh,
            },
            formula_raster::Placement {
                image: notes_image,
                x: 0.0,
                y: bh + gap,
                width,
                height,
            },
        ],
    )
    .ok_or("Cannot compose formula notes")?;
    Ok((image, width, total_height))
}

/// Called on preview workers. Uses the same notes geometry as the reader image.
pub fn preview(latex: &str, notes: &[FormulaNote]) -> Result<RasterResource, String> {
    let options = resvg::usvg::Options {
        fontdb: svg_font_database(),
        ..Default::default()
    };
    let typography = ReaderTypography {
        font_size: 32.0,
        ..Default::default()
    };
    let color = Rgba {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 255,
    };
    let formula = formula_raster::prepare(
        &MathRun {
            latex: latex.into(),
            display: true,
            original: None,
            size_scale: 1.0,
        },
        &typography,
        color,
        776.0,
        &options,
        None,
    )?;
    let body = super::formula_images::padded_display_row(
        formula.into_tuple(),
        None,
        776.0,
        8192.0,
        12.0,
        12.0,
        false,
    )
    .ok_or("Cannot pad formula")?;
    let (image, _, _) = append(body, notes, 800.0, &typography, color, &options, None)?;
    Ok(RasterResource {
        width: image.width,
        height: image.height,
        pixels: image.pixels,
    })
}

impl LayoutEngine {
    pub(super) fn push_noted_formula(
        &mut self,
        paginator: &mut Paginator,
        source: &dyn BookSource,
        block: &TextBlock,
        style: &ReaderStyle,
        width: f32,
    ) -> Result<bool, LayoutError> {
        if !self.formula_notes_visible {
            return Ok(false);
        }
        if !block
            .content
            .iter()
            .all(|inline| matches!(inline, Inline::Math(_) | Inline::Break))
        {
            return Ok(false);
        }
        let Some(origin) = block.source.as_ref() else {
            return Ok(false);
        };
        let mut math = block.content.iter().filter_map(|inline| {
            if let Inline::Math(math) = inline {
                Some(math)
            } else {
                None
            }
        });
        let Some(run) = math.next() else {
            return Ok(false);
        };
        if math.next().is_some() {
            return Ok(false);
        }
        let notes = source.visible_formula_notes(origin, &run.latex);
        if notes.is_empty() {
            return Ok(false);
        }
        let deferred = self
            .deferred_images
            .then_some((source, self.raster_generation));
        let mut display = run.clone();
        display.display = true;
        let Ok(formula) = formula_raster::prepare(
            &display,
            &style.typography,
            style.foreground,
            width,
            &self.svg_options,
            deferred,
        ) else {
            return Ok(false);
        };
        let original = formula.image.clone();
        let Ok((raster, w, h)) = append(
            formula.into_tuple(),
            &notes,
            width,
            &style.typography,
            style.foreground,
            &self.svg_options,
            deferred,
        ) else {
            return Ok(false);
        };
        let gap = style.typography.font_size * style.typesetting.media_gap_em;
        paginator.push_image(
            raster,
            ImageStyle {
                width: Some(ImageLength::Pixels(w)),
                height: Some(ImageLength::Pixels(h)),
                margin_before: gap,
                margin_after: gap,
                ..Default::default()
            },
            Some(origin.clone()),
            None,
        );
        if let Some(PageItem::Image(image)) = paginator.items.last_mut() {
            image.formula_presentation = Some(FormulaPresentation {
                original,
                latex: run.latex.clone(),
            });
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gpc_notes_render_into_one_image_with_aligned_columns() {
        let notes = [
            FormulaNote {
                symbol: r"\mathrm{GPC}".into(),
                meaning: "每字符手势数（gestures per character）".into(),
                unit: String::new(),
            },
            FormulaNote {
                symbol: r"|I S_{\varnothing}|".into(),
                meaning: "编码所有字符、退格、未识别及其它动作的输入流长度/手势数".into(),
                unit: String::new(),
            },
            FormulaNote {
                symbol: "|T|".into(),
                meaning: "转录字符串中的字符数".into(),
                unit: String::new(),
            },
        ];
        let mut wide_column_x: Option<f32> = None;
        for width in [240.0, 800.0, 1600.0] {
            let (svg, height) = geometry(
                &notes,
                width,
                20.0,
                Rgba {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 255,
                },
            )
            .unwrap();
            assert!(height > 60.0);
            assert!(!svg.contains(r"\mathrm"));
            let tree = resvg::usvg::Tree::from_str(
                &svg,
                &resvg::usvg::Options {
                    fontdb: svg_font_database(),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(tree.size().width() <= width + 0.1);
            let positions = svg
                .split("<text x=\"")
                .skip(1)
                .map(|row| row.split('"').next().unwrap().parse::<f32>().unwrap())
                .collect::<Vec<_>>();
            let x = positions[0];
            assert!(
                positions
                    .iter()
                    .all(|position| (*position - x).abs() < 0.01)
            );
            if width >= 800.0 {
                assert!(
                    x < 150.0,
                    "short symbols must not reserve a fraction of the whole image"
                );
                if let Some(previous) = wide_column_x {
                    assert!((previous - x).abs() < 0.01_f32);
                }
                wide_column_x = Some(x);
            }
        }
        let bare = preview(r"\mathrm{GPC}=\frac{|I S_{\varnothing}|}{|T|}", &[]).unwrap();
        let annotated = preview(r"\mathrm{GPC}=\frac{|I S_{\varnothing}|}{|T|}", &notes).unwrap();
        assert!(annotated.height > bare.height);
        let below = &annotated.pixels[(bare.height as usize * annotated.width as usize * 4)..];
        assert!(
            below.chunks_exact(4).any(|p| p[3] > 128),
            "notes are part of the image pixels"
        );
        if let Some(path) = std::env::var_os("TORTO_FORMULA_NOTES_PREVIEW") {
            let pixels = resvg::tiny_skia::Pixmap::from_vec(
                annotated.pixels.to_vec(),
                resvg::tiny_skia::IntSize::from_wh(annotated.width, annotated.height).unwrap(),
            )
            .unwrap();
            let mut paper =
                resvg::tiny_skia::Pixmap::new(annotated.width, annotated.height).unwrap();
            paper.fill(resvg::tiny_skia::Color::WHITE);
            paper.draw_pixmap(
                0,
                0,
                pixels.as_ref(),
                &Default::default(),
                resvg::tiny_skia::Transform::identity(),
                None,
            );
            paper.save_png(path).unwrap();
        }
    }
}
