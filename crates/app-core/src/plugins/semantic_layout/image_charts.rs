//! Faithful white-background vector redraws for legible raster line charts.
use super::*;
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Trace {
    height: u16,
    axes: [u16; 5],
    x_labels: Vec<Label>,
    y_labels: Vec<Label>,
    x_unit: String,
    y_unit: String,
    series: Vec<Series>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Label {
    position: u16,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Series {
    name: String,
    heavy: bool,
    points: Vec<[u16; 2]>,
}

pub(super) fn schema() -> Value {
    let coord = json!({"type":"integer"});
    let coords = json!({"type":"array","items":coord});
    let label = json!({"type":"object","additionalProperties":false,"required":["position","text"],"properties":{"position":coord,"text":{"type":"string"}}});
    json!({"type":["object","null"],"additionalProperties":false,
        "required":["height","axes","x_labels","y_labels","x_unit","y_unit","series"],
        "properties":{"height":coord,"axes":coords,"x_labels":{"type":"array","items":label},
        "y_labels":{"type":"array","items":label},"x_unit":{"type":"string"},"y_unit":{"type":"string"},
        "series":{"type":"array","items":{"type":"object","additionalProperties":false,
        "required":["name","heavy","points"],"properties":{"name":{"type":"string"},"heavy":{"type":"boolean"},
        "points":{"type":"array","items":coords}}}}}})
}

pub(super) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn short(s: &str, limit: usize) -> bool {
    s.chars().count() <= limit && !s.chars().any(char::is_control)
}

impl Trace {
    /// Check both traced curves against the same padded image sent to vision.
    /// A syntactically valid but misplaced trace must never replace a figure.
    pub(super) fn matches_raster(&self, image: &image::GrayImage) -> bool {
        let [width, height] = [
            image.width().saturating_sub(16),
            image.height().saturating_sub(16),
        ];
        if width < 180
            || height < 100
            || self.svg().is_none()
            || (height as f32 / width as f32 - f32::from(self.height) / 1000.0).abs() > 0.08
        {
            return false;
        }
        let [left, top, right, bottom, axis_y] = self.axes;
        let has_ink = |x: f32, y: f32| {
            let px = (8.0 + x / 1000.0 * width as f32).round() as i32;
            let py = (8.0 + y / f32::from(self.height) * height as f32).round() as i32;
            let radius = (width as i32 / 85).clamp(4, 17);
            (-radius..=radius).any(|dy| {
                (-radius..=radius).any(|dx| {
                    let [x, y] = [px + dx, py + dy];
                    dx * dx + dy * dy <= radius * radius
                        && x >= 8
                        && y >= 8
                        && (x as u32) < width + 8
                        && (y as u32) < height + 8
                        && image.get_pixel(x as u32, y as u32).0[0] < 150
                })
            })
        };
        self.series.iter().all(|curve| {
            let (mut good, mut checked) = (0, 0);
            for pair in curve.points.windows(2) {
                let ([[x0, y0], [x1, y1]], samples) =
                    ([pair[0].map(f32::from), pair[1].map(f32::from)], 6);
                for j in 0..=samples {
                    let t = j as f32 / samples as f32;
                    let (x, y) = ((x1 - x0) * t + x0, (y1 - y0) * t + y0);
                    if x < f32::from(left) + 18.0
                        || x > f32::from(right) - 8.0
                        || y < f32::from(top) + 5.0
                        || y > f32::from(bottom) - 5.0
                        || (y - f32::from(axis_y)).abs() < 16.0
                    {
                        continue;
                    }
                    checked += 1;
                    good += usize::from(has_ink(x, y));
                }
            }
            checked >= 4 && good * 5 >= checked * 4
        })
    }

    pub(super) fn svg(&self) -> Option<String> {
        let [left, top, right, bottom, axis_y] = self.axes;
        let h = self.height;
        if !(300..=1050).contains(&h)
            || left < 45
            || right > 975
            || right.saturating_sub(left) < 320
            || top < 28
            || bottom > h.saturating_sub(82)
            || bottom.saturating_sub(top) < 130
            || !(top..=bottom).contains(&axis_y)
            || !(1..=2).contains(&self.series.len())
            || !short(&self.x_unit, 24)
            || !short(&self.y_unit, 24)
            || !labels_ok(&self.x_labels, left, right, 28)
            || !labels_ok(&self.y_labels, top, bottom, 16)
        {
            return None;
        }
        if self.series.len() == 2
            && (self.series[0].name == self.series[1].name
                || self.series[0].heavy == self.series[1].heavy)
        {
            return None;
        }
        for s in &self.series {
            if !short(&s.name, 36)
                || (self.series.len() > 1 && s.name.trim().is_empty())
                || !(2..=72).contains(&s.points.len())
                || s.points
                    .iter()
                    .any(|[x, y]| !(left..=right).contains(x) || !(top..=bottom).contains(y))
                || s.points.windows(2).any(|p| p[0][0] >= p[1][0])
            {
                return None;
            }
        }
        let mut svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="{h}" viewBox="0 0 1000 {h}"><rect width="1000" height="{h}" fill="#fff"/><g stroke="#252525" fill="none" stroke-width="2"><path d="M{left} {top}V{bottom} M{left} {axis_y}H{right}"/>"##
        );
        for Label { position, .. } in &self.y_labels {
            let _ = write!(svg, r#"<path d="M{} {position}h7"/>"#, left - 7);
        }
        for Label { position, .. } in &self.x_labels {
            let _ = write!(svg, r#"<path d="M{position} {axis_y}v7"/>"#);
        }
        svg.push_str("</g>");
        for s in &self.series {
            let width = if s.heavy { 4.5 } else { 2.0 };
            let points = s
                .points
                .iter()
                .map(|[x, y]| format!("{x},{y}"))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(
                svg,
                r##"<polyline points="{points}" stroke="#202020" stroke-width="{width}" stroke-linejoin="round" fill="none"/>"##
            );
        }
        svg.push_str(r##"<g fill="#151515" font-size="18" font-family="sans-serif">"##);
        for Label { position, text } in &self.y_labels {
            let _ = write!(
                svg,
                r#"<text x="{}" y="{}" text-anchor="end">{}</text>"#,
                left - 10,
                position + 6,
                escape(text)
            );
        }
        for Label { position, text } in &self.x_labels {
            let y = bottom + 25;
            let _ = write!(
                svg,
                r#"<text x="{position}" y="{y}" transform="rotate(-38 {position} {y})" text-anchor="end">{}</text>"#,
                escape(text)
            );
        }
        let _ = write!(
            svg,
            r#"<text x="{left}" y="{}">{}</text><text x="{right}" y="{}" text-anchor="end">{}</text>"#,
            top - 10,
            escape(&self.y_unit),
            bottom + 46,
            escape(&self.x_unit)
        );
        for (index, s) in self
            .series
            .iter()
            .filter(|s| !s.name.is_empty())
            .enumerate()
        {
            let x = 100 + index * 435;
            let y = h - 15;
            let width = if s.heavy { 4.5 } else { 2.0 };
            let _ = write!(
                svg,
                r##"<path d="M{x} {}h38" stroke="#202020" stroke-width="{width}"/><text x="{}" y="{y}">{}</text>"##,
                y - 6,
                x + 48,
                escape(&s.name)
            );
        }
        svg.push_str("</g></svg>");
        Some(svg)
    }
}

fn labels_ok(labels: &[Label], min: u16, max: u16, limit: usize) -> bool {
    (2..=limit).contains(&labels.len())
        && labels.iter().all(|l| {
            (min..=max).contains(&l.position) && !l.text.trim().is_empty() && short(&l.text, 16)
        })
        && labels
            .windows(2)
            .all(|pair| pair[0].position < pair[1].position)
}

pub(super) fn compose(blocks: &mut [Block], original: &PublicationUrl, svg: &str) {
    let generated = enhancements::chart_href(svg);
    fn replace(
        img: &mut rebook_publication::ImageBlock,
        original: &PublicationUrl,
        generated: &PublicationUrl,
    ) {
        if &img.href == original && img.text_layer.is_none() {
            img.href = generated.clone();
        }
    }
    for block in blocks {
        match block {
            Block::Image(img) => replace(img, original, &generated),
            Block::Figure(f) => {
                for img in &mut f.images {
                    replace(img, original, &generated);
                }
                compose(&mut f.body, original, svg);
            }
            Block::Note(n) => compose(&mut n.blocks, original, svg),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traces_preserve_legend_and_validate_against_source_ink() {
        let data = json!({
            "height":500, "axes":[90,50,920,380,250],
            "x_labels":[{"position":90,"text":"1991"},{"position":920,"text":"2009"}],
            "y_labels":[{"position":50,"text":"35"},{"position":380,"text":"-20"}],
            "x_unit":"年","y_unit":"%", "series":[
                {"name":"World & China","heavy":false,"points":[[150,315],[480,210],[880,115]]},
                {"name":"China","heavy":true,"points":[[330,290],[530,145],[905,190]]}
            ]
        });
        let trace: Trace = serde_json::from_value(wire::decode(&wire::encode(&data))).unwrap();
        let svg = trace.svg().unwrap();
        assert!(svg.contains("World &amp; China"));
        assert!(svg.contains("-20"));
        assert!(svg.contains("330,290 530,145"));
        assert!(resvg::usvg::Tree::from_str(&svg, &Default::default()).is_ok());
        let mut image = image::GrayImage::from_pixel(1016, 516, image::Luma([255]));
        assert!(!trace.matches_raster(&image));
        for curve in &trace.series {
            for pair in curve.points.windows(2) {
                let [[x0, y0], [x1, y1]] = [pair[0].map(f32::from), pair[1].map(f32::from)];
                for i in 0..=500 {
                    let t = i as f32 / 500.0;
                    let (x, y) = (
                        ((x1 - x0) * t + x0).round() as u32 + 8,
                        ((y1 - y0) * t + y0).round() as u32 + 8,
                    );
                    image.put_pixel(x, y, image::Luma([0]));
                }
            }
        }
        assert!(trace.matches_raster(&image));
        let mut wrong = trace;
        wrong.series[1].points = vec![[330, 350], [530, 360], [905, 370]];
        assert!(!wrong.matches_raster(&image));
    }
}

#[cfg(test)]
#[test]
#[ignore = "requires TORTO_STEEL_CHART_MOBI; uses a verified local line trace, without a provider"]
fn local_steel_chart_accepts_the_two_original_curves() {
    let path = std::env::var_os("TORTO_STEEL_CHART_MOBI").unwrap();
    let source = rebook_formats::open_file(path).unwrap().source();
    let href = PublicationUrl::parse("Images/kindle-36.jpg").unwrap();
    let image = image::load_from_memory(&source.resource(&href).unwrap().bytes)
        .unwrap()
        .into_luma8();
    assert_eq!(image.dimensions(), (1070, 574));
    let mut padded = image::GrayImage::from_pixel(1086, 590, image::Luma([255]));
    image::imageops::overlay(&mut padded, &image, 8, 8);
    let pixels = |u: u16| ((f32::from(u) * 1000.0 / 1070.0).round()) as u16;
    let series = |name: &str, heavy: bool, start: u16, values: &[u16]| Series {
        name: name.into(),
        heavy,
        points: values
            .iter()
            .enumerate()
            .map(|(i, &y)| [pixels(120 + 47 * (start + i as u16)), pixels(y)])
            .collect(),
    };
    let trace = Trace {
        height: pixels(574),
        axes: [97, 82, 982, 427, 302].map(pixels),
        x_labels: vec![
            Label {
                position: pixels(120),
                text: "1991".into(),
            },
            Label {
                position: pixels(960),
                text: "2009".into(),
            },
        ],
        y_labels: vec![
            Label {
                position: pixels(82),
                text: "35".into(),
            },
            Label {
                position: pixels(427),
                text: "-20".into(),
            },
        ],
        x_unit: "年".into(),
        y_unit: "%".into(),
        series: vec![
            series(
                "世界粗钢产量增速",
                false,
                0,
                &[
                    318, 361, 245, 303, 275, 303, 262, 317, 294, 256, 303, 260, 256, 241, 254, 238,
                    250, 315, 356,
                ],
            ),
            series(
                "中国粗钢产量增速",
                true,
                4,
                &[
                    296, 258, 254, 265, 251, 287, 218, 139, 162, 159, 117, 171, 206, 287, 217,
                ],
            ),
        ],
    };
    assert!(trace.matches_raster(&padded));
    let svg = trace.svg().unwrap();
    assert!(svg.contains("世界粗钢产量增速") && svg.contains("中国粗钢产量增速"));
}
