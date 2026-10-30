//! Small deterministic SVG renderer: values are resolved from source cells, not
//! model-generated drawing code. A zero baseline supports positive/negative bars.
use std::fmt::Write as _;

pub(super) struct Panel {
    pub title: String,
    pub unit: String,
    pub categories: Vec<String>,
    pub series: Vec<(String, Vec<f64>)>,
}
const COLORS: [&str; 6] = [
    "#39886b", "#5483b5", "#b98047", "#8865a3", "#b65d66", "#699a9d",
];
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn text(svg: &mut String, x: f64, y: f64, value: &str, anchor: &str, size: u32) {
    let _ = write!(
        svg,
        "<text x='{x:.2}' y='{y:.2}' text-anchor='{anchor}' font-family='sans-serif' font-size='{size}' fill='#26352e'>{}</text>",
        escape(value)
    );
}
fn number(value: f64) -> String {
    if value.abs() >= 1e7 || value != 0.0 && value.abs() < 0.001 {
        format!("{value:.2e}")
    } else {
        format!("{value:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .into()
    }
}
fn lines(value: &str, limit: usize) -> Vec<String> {
    value
        .chars()
        .collect::<Vec<_>>()
        .chunks(limit)
        .map(|c| c.iter().collect())
        .collect()
}
fn panel_title(panel: &Panel) -> String {
    if panel.unit.is_empty() {
        panel.title.clone()
    } else {
        format!("{} ({})", panel.title, panel.unit)
    }
}
fn legend_lines(panel: &Panel) -> usize {
    panel
        .series
        .iter()
        .map(|(name, _)| name.chars().count().div_ceil(18))
        .max()
        .unwrap_or(1)
}
pub(super) fn render(kind: &str, title: &str, panels: &[Panel]) -> String {
    let panel_height = |p: &Panel| {
        let title_extra = lines(&panel_title(p), 45).len().saturating_sub(1) * 22;
        title_extra
            + if kind == "pie" {
                pie_height(p)
            } else {
                350 + category_lines(p) * 16
                    + p.series.len().div_ceil(3) * (legend_lines(p) * 16 + 12)
                    + 30
            }
    };
    let title_lines = lines(title, 40);
    let header = 44 + title_lines.len() * 26;
    let height = header + panels.iter().map(panel_height).sum::<usize>();
    let mut svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='960' height='{height}' viewBox='0 0 960 {height}'><rect width='960' height='{height}' rx='12' fill='#fcfbf7'/>"
    );
    for (i, line) in title_lines.iter().enumerate() {
        text(&mut svg, 480.0, 35.0 + i as f64 * 26.0, line, "middle", 22);
    }
    let mut top = header as f64;
    for panel in panels {
        let title = lines(&panel_title(panel), 45);
        for (i, line) in title.iter().enumerate() {
            text(&mut svg, 90.0, top + i as f64 * 22.0, line, "start", 17);
        }
        let content_top = top + title.len().saturating_sub(1) as f64 * 22.0;
        if kind == "pie" {
            pie(&mut svg, panel, content_top);
        } else {
            xy(&mut svg, kind, panel, content_top);
        }
        top += panel_height(panel) as f64;
    }
    svg.push_str("</svg>");
    svg
}
fn category_lines(panel: &Panel) -> usize {
    let limit = ((820.0 / panel.categories.len().max(1) as f64 / 13.0) as usize).clamp(3, 16);
    panel
        .categories
        .iter()
        .map(|s| s.chars().count().div_ceil(limit))
        .max()
        .unwrap_or(1)
}
fn xy(svg: &mut String, kind: &str, panel: &Panel, top: f64) {
    let values = panel.series.iter().flat_map(|(_, v)| v.iter().copied());
    let min = values.clone().fold(0.0, f64::min);
    let max = values.fold(0.0, f64::max);
    let range = (max - min).max(1e-12);
    let (left, width, bottom, height) = (90.0, 820.0, top + 280.0, 235.0);
    let y = |value: f64| bottom - (value - min) / range * height;
    let zero = y(0.0);
    for tick in 0..=4 {
        let value = min + range * tick as f64 / 4.0;
        let y = y(value);
        let _ = write!(
            svg,
            "<path d='M {left} {y:.2} h {width}' stroke='#e1e5df' fill='none'/>"
        );
        text(svg, left - 10.0, y + 5.0, &number(value), "end", 13);
    }
    let _ = write!(
        svg,
        "<path d='M {left} {zero:.2} h {width}' stroke='#64766b' fill='none'/>"
    );
    let step = width / panel.categories.len() as f64;
    let bar_width = (step * 0.72 / panel.series.len() as f64).min(80.0);
    for (series, (name, values)) in panel.series.iter().enumerate() {
        let color = COLORS[series % COLORS.len()];
        let mut points = String::new();
        for (i, value) in values.iter().enumerate() {
            let center = left + (i as f64 + 0.5) * step;
            let py = y(*value);
            if kind == "bar" {
                let x = center + bar_width * (series as f64 - panel.series.len() as f64 / 2.0);
                let _ = write!(
                    svg,
                    "<rect x='{x:.2}' y='{:.2}' width='{bar_width:.2}' height='{:.2}' fill='{color}'><title>{}: {}</title></rect>",
                    py.min(zero),
                    (zero - py).abs(),
                    escape(name),
                    value
                );
            } else {
                let _ = write!(points, "{center:.2},{py:.2} ");
                let _ = write!(
                    svg,
                    "<circle cx='{center:.2}' cy='{py:.2}' r='3' fill='{color}'><title>{}: {}</title></circle>",
                    escape(name),
                    value
                );
            }
        }
        if kind == "line" {
            let _ = write!(
                svg,
                "<polyline points='{points}' fill='none' stroke='{color}' stroke-width='2.5'/>"
            );
        }
        let x = 90.0 + (series % 3) as f64 * 275.0;
        let ly = top
            + 330.0
            + category_lines(panel) as f64 * 16.0
            + (series / 3) as f64 * (legend_lines(panel) * 16 + 12) as f64;
        let _ = write!(
            svg,
            "<rect x='{x}' y='{}' width='14' height='14' fill='{color}'/>",
            ly - 12.0
        );
        for (i, line) in lines(name, 18).iter().enumerate() {
            text(svg, x + 21.0, ly + i as f64 * 16.0, line, "start", 14);
        }
    }
    for (i, label) in panel.categories.iter().enumerate() {
        // Bounded multiline labels avoid losing CJK category names at narrow widths.
        let chars = label.chars().collect::<Vec<_>>();
        let limit = ((step / 13.0) as usize).clamp(3, 16);
        for (line, part) in chars.chunks(limit).enumerate() {
            text(
                svg,
                left + (i as f64 + 0.5) * step,
                bottom + 22.0 + line as f64 * 16.0,
                &part.iter().collect::<String>(),
                "middle",
                12,
            );
        }
    }
}
fn pie_height(panel: &Panel) -> usize {
    (65 + panel
        .categories
        .iter()
        .map(|s| (s.chars().count() + 14).div_ceil(40) * 16 + 10)
        .sum::<usize>())
    .max(420)
}
fn pie(svg: &mut String, panel: &Panel, top: f64) {
    let values = &panel.series[0].1;
    let sum = values.iter().sum::<f64>();
    let (cx, cy, r) = (300.0, top + 190.0, 130.0);
    let mut angle = -std::f64::consts::FRAC_PI_2;
    let mut label_y = top + 35.0;
    for (i, value) in values.iter().enumerate() {
        let end = angle + value / sum * std::f64::consts::TAU;
        let color = COLORS[i % COLORS.len()];
        if *value / sum >= 0.999999 {
            let _ = write!(svg, "<circle cx='{cx}' cy='{cy}' r='{r}' fill='{color}'/>");
        } else if *value > 0.0 {
            let _ = write!(
                svg,
                "<path d='M {cx} {cy} L {:.2} {:.2} A {r} {r} 0 {} 1 {:.2} {:.2} Z' fill='{color}'/>",
                cx + r * angle.cos(),
                cy + r * angle.sin(),
                u8::from(end - angle > std::f64::consts::PI),
                cx + r * end.cos(),
                cy + r * end.sin()
            );
        }
        let x = 490.0;
        let _ = write!(
            svg,
            "<rect x='{x}' y='{}' width='12' height='12' fill='{color}'/>",
            label_y - 11.0
        );
        let label = format!("{}: {}", panel.categories[i], number(*value));
        for line in label.chars().collect::<Vec<_>>().chunks(40) {
            text(
                svg,
                x + 18.0,
                label_y,
                &line.iter().collect::<String>(),
                "start",
                13,
            );
            label_y += 16.0;
        }
        label_y += 10.0;
        angle = end;
    }
}

// The reader requests dimensions first and materializes this on its existing
// background raster worker. The shared reader raster cache owns the pixels.
pub(super) fn raster(
    svg: &[u8],
) -> Result<rebook_publication::RasterResource, rebook_publication::PublicationError> {
    let options = resvg::usvg::Options {
        fontdb: rebook_layout::svg_font_database(),
        ..Default::default()
    };
    let error = |e: String| rebook_publication::PublicationError::InvalidPublication(e);
    let tree = resvg::usvg::Tree::from_data(svg, &options).map_err(|e| error(e.to_string()))?;
    let [width, height] = [
        tree.size().width().ceil() as u32,
        tree.size().height().ceil() as u32,
    ];
    if width > 1920 || height > 8192 {
        return Err(error("Chart raster is too large".into()));
    }
    let mut pixels = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| error("Chart raster allocation failed".into()))?;
    resvg::render(
        &tree,
        resvg::usvg::Transform::identity(),
        &mut pixels.as_mut(),
    );
    Ok(rebook_publication::RasterResource {
        width,
        height,
        pixels: pixels.take().into(),
    })
}
