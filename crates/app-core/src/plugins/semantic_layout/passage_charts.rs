//! Explicit, source-backed visualizations. No chapter scan schedules these.
use super::*;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;

#[derive(Clone)]
pub struct PassageChart {
    source: SourceRange,
    input: String,
    light_svg: String,
    dark_svg: String,
}

#[derive(Default)]
pub(super) struct Views {
    entries: Vec<(PassageChart, bool)>,
    dark: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DiagramKind {
    #[default]
    Flow,
    Timeline,
    Compare,
    Hierarchy,
    Metric,
    Summary,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RelationKind {
    #[default]
    Cause,
    Sequence,
    Contrast,
    Part,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Diagram {
    title: String,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    kind: DiagramKind,
    nodes: Vec<Node>,
    #[serde(default)]
    relations: Vec<Relation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    label: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    emphasis: Option<bool>,
    evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Relation {
    from: usize,
    to: usize,
    #[serde(default)]
    label: String,
    #[serde(default)]
    kind: RelationKind,
    evidence: String,
}

#[derive(Clone, Copy)]
struct Palette {
    bg: &'static str,
    card: &'static str,
    card_emphasis: &'static str,
    border: &'static str,
    border_emphasis: &'static str,
    ink: &'static str,
    muted: &'static str,
    accent: &'static str,
    accent_soft: &'static str,
    contrast: &'static str,
    contrast_soft: &'static str,
    badge_bg: &'static str,
    track: &'static str,
}

const LIGHT: Palette = Palette {
    bg: "#FAF9F6",
    card: "#FFFFFF",
    card_emphasis: "#EEF5F1",
    border: "#E2DFD7",
    border_emphasis: "#2A6B53",
    ink: "#1C2124",
    muted: "#646B70",
    accent: "#2A6B53",
    accent_soft: "#DCEEE5",
    contrast: "#B85C38",
    contrast_soft: "#F7E6DF",
    badge_bg: "#F2F0EA",
    track: "#EAE7DF",
};

const DARK: Palette = Palette {
    bg: "#1A1D21",
    card: "#24292F",
    card_emphasis: "#1E352D",
    border: "#363D47",
    border_emphasis: "#5CB594",
    ink: "#ECEFF4",
    muted: "#9BA3AE",
    accent: "#5CB594",
    accent_soft: "#233C33",
    contrast: "#E08E6D",
    contrast_soft: "#3D2922",
    badge_bg: "#2D333B",
    track: "#30363F",
};

fn same(a: &SourceRange, b: &SourceRange) -> bool {
    a.start.spine == b.start.spine && a.start.node == b.start.node
}

fn eligible_block(block: &Block) -> Option<(SourceRange, String)> {
    let eligible_kind = match block {
        Block::Text(t) => {
            matches!(
                t.kind,
                TextBlockKind::Paragraph | TextBlockKind::ListItem { .. }
            ) && t.source.is_some()
        }
        Block::Quote(_) => true,
        _ => false,
    };
    if !eligible_kind {
        return None;
    }
    let source = crate::reading_text::block_source_range(block)?.clone();
    let text = crate::reading_text::block_focus_text(block);
    let trimmed = text.trim();
    (16..=2600)
        .contains(&trimmed.chars().count())
        .then(|| (source, trimmed.to_owned()))
}

impl SemanticLayoutSource {
    pub fn set_passage_chart_dark(&self, dark: bool) -> bool {
        let Ok(mut views) = self.passage_charts.try_write() else {
            return false;
        };
        if views.dark == dark {
            return false;
        }
        views.dark = dark;
        views.entries.iter().any(|(_, active)| *active)
    }

    pub fn passage_chart_control(&self, source: &SourceRange) -> Option<bool> {
        self.passage_charts
            .try_read()
            .ok()?
            .entries
            .iter()
            .rev()
            .find(|(chart, _)| same(&chart.source, source))
            .map(|(_, active)| *active)
    }

    pub fn toggle_passage_chart(&self, source: &SourceRange) -> bool {
        let Ok(mut views) = self.passage_charts.try_write() else {
            return false;
        };
        let Some((_, active)) = views
            .entries
            .iter_mut()
            .rev()
            .find(|(c, _)| same(&c.source, source))
        else {
            return false;
        };
        *active = !*active;
        true
    }

    pub fn install_passage_chart(&self, chart: PassageChart) -> bool {
        let Ok(mut views) = self.passage_charts.try_write() else {
            return false;
        };
        views
            .entries
            .retain(|(previous, _)| !same(&previous.source, &chart.source));
        if views.entries.len() >= 32 {
            views.entries.remove(0);
        }
        views.entries.push((chart, true));
        true
    }

    pub fn clear_passage_charts(&self) {
        if let Ok(mut charts) = self.passage_charts.try_write() {
            charts.entries.clear();
        }
    }

    pub(super) fn apply_passage_charts(&self, blocks: &mut Vec<Block>) {
        let Ok(charts) = self.passage_charts.read() else {
            return;
        };
        if charts.entries.is_empty() {
            return;
        }
        let dark = charts.dark;
        let mut index = 0;
        while index < blocks.len() {
            let replacement = eligible_block(&blocks[index]).and_then(|(source, prose)| {
                let chart = charts
                    .entries
                    .iter()
                    .rev()
                    .find(|(chart, active)| *active && same(&chart.source, &source))?
                    .0
                    .clone();
                if digest(prose.as_bytes()) != chart.input {
                    return None;
                }
                let svg = if dark {
                    &chart.dark_svg
                } else {
                    &chart.light_svg
                };
                Some(Block::Image(rebook_publication::ImageBlock {
                    ocr_text: None,
                    formula_image: false,
                    formula: None,
                    href: enhancements::chart_href(svg),
                    alt: prose,
                    style: Default::default(),
                    source: Some(source),
                    text_layer: None,
                }))
            });
            if let Some(block) = replacement {
                blocks[index] = block;
                // A source-less immediate companion belongs to this paragraph.
                if matches!(blocks.get(index + 1), Some(Block::Text(t)) if t.source.is_none()) {
                    blocks.remove(index + 1);
                }
            }
            index += 1;
        }
    }

    pub(super) fn passage_resource(&self, href: &PublicationUrl) -> Option<Resource> {
        let views = self.passage_charts.read().ok()?;
        let svg = views.entries.iter().find_map(|(c, _)| {
            if enhancements::chart_href(&c.light_svg) == *href {
                Some(&c.light_svg)
            } else if enhancements::chart_href(&c.dark_svg) == *href {
                Some(&c.dark_svg)
            } else {
                None
            }
        })?;
        Some(Resource {
            href: href.clone(),
            media_type: "image/svg+xml".into(),
            bytes: Arc::from(svg.as_bytes()),
        })
    }

    pub async fn visualize_passage(
        self: Arc<Self>,
        index: usize,
        target: SourceRange,
        settings: &PluginSettings,
    ) -> Result<Option<PassageChart>, String> {
        let source = self.inner.clone();
        let (range, prose) = tokio::task::spawn_blocking(move || {
            let section = source.parse_section(index).ok()?;
            section
                .blocks
                .iter()
                .filter_map(eligible_block)
                .find(|(s, _)| same(s, &target))
        })
        .await
        .map_err(|e| e.to_string())?
        .ok_or("This block has no eligible prose")?;
        let (provider, model, effort) = settings
            .semantic_layout_image_endpoint()
            .ok_or("请配置排版生成模型")?;
        let client = crate::http::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .map_err(|e| e.to_string())?;
        let instructions = wire::prompt(include_str!("passage_charts_prompt.md"));
        let messages = [
            json!({"role":"system","content":instructions}),
            json!({"role":"user","content":json!({"passage":prose}).to_string()}),
        ];
        let response = crate::plugins::llm::budgeted(ai::request_completion(
            &client,
            provider,
            model,
            &messages,
            None,
            Some(3072),
            effort,
            Some(&json!({"temperature":0})),
        ))
        .await?;
        let content = ai::message_content(&response).ok_or("No diagram was returned")?;
        let raw = llm_json::parse::<Value>(&content)?;
        let diagram: Diagram =
            serde_json::from_value(wire::decode(&raw)).map_err(|e| e.to_string())?;
        Ok(diagram.render_pair(&prose).map(|(light_svg, dark_svg)| {
            PassageChart {
                source: range,
                input: digest(prose.as_bytes()),
                light_svg,
                dark_svg,
            }
        }))
    }
}

fn char_width_em(c: char) -> f32 {
    if c.is_ascii() {
        if c == ' ' || matches!(c, '.' | ',' | ':' | ';' | '!' | '|' | 'i' | 'l' | 'I') {
            0.34
        } else if c.is_ascii_uppercase() || matches!(c, 'm' | 'w' | 'M' | 'W' | '@' | '%') {
            0.66
        } else {
            0.54
        }
    } else {
        1.0
    }
}

fn estimate_width_px(text: &str, font_size: f32) -> f32 {
    text.chars().map(char_width_em).sum::<f32>() * font_size
}

fn wrap_text(text: &str, max_width_px: f32, font_size: f32, max_lines: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || max_lines == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_w = 0.0f32;
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            if !current.trim().is_empty() {
                lines.push(current.trim().to_owned());
                current.clear();
                current_w = 0.0;
                if lines.len() >= max_lines {
                    break;
                }
            }
            continue;
        }
        let cw = char_width_em(c) * font_size;
        if current_w + cw > max_width_px && !current.is_empty() {
            // Try breaking on last ASCII space for Latin prose.
            let mut next_carry = String::new();
            if c.is_ascii_alphanumeric()
                && let Some(pos) = current.rfind(' ')
                && pos > current.len() / 3
            {
                next_carry = current[pos + 1..].to_owned();
                current.truncate(pos);
            }
            lines.push(current.trim().to_owned());
            if lines.len() >= max_lines {
                if let Some(last) = lines.last_mut()
                    && !last.ends_with('…')
                {
                    let mut clipped: Vec<char> = last.chars().collect();
                    if clipped.len() > 1 {
                        clipped.pop();
                    }
                    *last = format!("{}…", clipped.into_iter().collect::<String>());
                }
                return lines;
            }
            current = next_carry;
            current_w = estimate_width_px(&current, font_size);
            if c == ' ' && current.is_empty() {
                continue;
            }
        }
        current.push(c);
        current_w += cw;
    }
    if !current.trim().is_empty() && lines.len() < max_lines {
        lines.push(current.trim().to_owned());
    }
    lines
}

fn write_text_lines(
    svg: &mut String,
    lines: &[String],
    x: f32,
    start_y: f32,
    line_height: f32,
    font_size: f32,
    fill: &str,
    weight: &str,
    anchor: &str,
) {
    for (i, line) in lines.iter().enumerate() {
        let y = start_y + i as f32 * line_height;
        let _ = write!(
            svg,
            r#"<text x="{x:.1}" y="{y:.1}" font-size="{font_size:.1}" font-weight="{weight}" fill="{fill}" text-anchor="{anchor}">{}</text>"#,
            image_charts::escape(line)
        );
    }
}

fn write_pill(
    svg: &mut String,
    cx: f32,
    cy: f32,
    text: &str,
    font_size: f32,
    bg: &str,
    fg: &str,
    border: Option<&str>,
) {
    let w = (estimate_width_px(text, font_size) + 18.0).clamp(32.0, 220.0);
    let h = font_size + 10.0;
    let x = cx - w / 2.0;
    let y = cy - h / 2.0;
    let stroke_attr = border
        .map(|b| format!(r#" stroke="{b}" stroke-width="1""#))
        .unwrap_or_default();
    let _ = write!(
        svg,
        r#"<rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="{:.1}" fill="{bg}"{stroke_attr}/><text x="{cx:.1}" y="{:.1}" font-size="{font_size:.1}" font-weight="600" fill="{fg}" text-anchor="middle">{}</text>"#,
        h / 2.0,
        cy + font_size * 0.35,
        image_charts::escape(text)
    );
}

#[derive(Clone, Copy)]
struct BoxRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl BoxRect {
    fn cx(self) -> f32 {
        self.x + self.w * 0.5
    }
    fn cy(self) -> f32 {
        self.y + self.h * 0.5
    }
    fn bottom(self) -> f32 {
        self.y + self.h
    }
    fn right(self) -> f32 {
        self.x + self.w
    }
}

impl Diagram {
    fn validate(&self, prose: &str) -> bool {
        fn text(s: &str, max: usize) -> bool {
            !s.trim().is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
        }
        fn opt_text(s: &Option<String>, max: usize) -> bool {
            s.as_deref()
                .map_or(true, |v| v.trim().is_empty() || text(v, max))
        }
        let evidence = |s: &str| text(s, 320) && s.chars().count() >= 4 && prose.contains(s);
        if !text(&self.title, 36)
            || !opt_text(&self.subtitle, 72)
            || !(2..=6).contains(&self.nodes.len())
            || self.relations.len() > 8
        {
            return false;
        }
        if self.nodes.iter().any(|n| {
            !text(&n.label, 42)
                || !opt_text(&n.detail, 90)
                || !opt_text(&n.tag, 28)
                || n.value.is_some_and(|v| !v.is_finite() || v < 0.0)
                || !evidence(&n.evidence)
        }) {
            return false;
        }
        if self.relations.iter().any(|r| {
            r.from == r.to
                || r.from >= self.nodes.len()
                || r.to >= self.nodes.len()
                || (!r.label.trim().is_empty() && !text(&r.label, 34))
                || !evidence(&r.evidence)
        }) {
            return false;
        }
        true
    }

    fn render_pair(&self, prose: &str) -> Option<(String, String)> {
        if !self.validate(prose) {
            return None;
        }
        Some((self.render_svg(LIGHT), self.render_svg(DARK)))
    }

    #[cfg(test)]
    fn svg(&self, prose: &str) -> Option<String> {
        self.render_pair(prose).map(|(light, _)| light)
    }

    fn render_svg(&self, p: Palette) -> String {
        const W: f32 = 760.0;
        const PAD: f32 = 28.0;
        let title_lines = wrap_text(&self.title, W - PAD * 2.0 - 96.0, 21.0, 2);
        let subtitle_lines = self
            .subtitle
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| wrap_text(s, W - PAD * 2.0, 13.5, 2))
            .unwrap_or_default();
        let mut header_h = PAD + title_lines.len() as f32 * 27.0;
        if !subtitle_lines.is_empty() {
            header_h += 4.0 + subtitle_lines.len() as f32 * 19.0;
        }
        let content_top = header_h + 18.0;

        let mut body = String::new();
        let content_bottom = match self.kind {
            DiagramKind::Flow => self.render_flow(&mut body, p, W, PAD, content_top),
            DiagramKind::Timeline => self.render_timeline(&mut body, p, W, PAD, content_top),
            DiagramKind::Compare => self.render_compare(&mut body, p, W, PAD, content_top),
            DiagramKind::Hierarchy => self.render_hierarchy(&mut body, p, W, PAD, content_top),
            DiagramKind::Metric => self.render_metric(&mut body, p, W, PAD, content_top),
            DiagramKind::Summary => self.render_summary(&mut body, p, W, PAD, content_top),
        };
        let total_h = (content_bottom + PAD).ceil();

        let kind_badge = match self.kind {
            DiagramKind::Flow => "FLOW",
            DiagramKind::Timeline => "TIMELINE",
            DiagramKind::Compare => "COMPARE",
            DiagramKind::Hierarchy => "STRUCTURE",
            DiagramKind::Metric => "METRICS",
            DiagramKind::Summary => "POINTS",
        };

        let mut svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{W:.0}" height="{total_h:.0}" viewBox="0 0 {W:.0} {total_h:.0}">"#,
        );
        let _ = write!(
            svg,
            r#"<defs><marker id="arr-accent" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="{}"/></marker><marker id="arr-contrast" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="{}"/></marker></defs>"#,
            p.accent, p.contrast
        );
        let _ = write!(
            svg,
            r#"<rect width="{W:.0}" height="{total_h:.0}" rx="16" fill="{}" stroke="{}" stroke-width="1"/>"#,
            p.bg, p.border
        );
        svg.push_str(
            r#"<g font-family="-apple-system, BlinkMacSystemFont, 'PingFang SC', 'Microsoft YaHei', sans-serif">"#,
        );
        write_text_lines(
            &mut svg,
            &title_lines,
            PAD,
            PAD + 20.0,
            27.0,
            21.0,
            p.ink,
            "700",
            "start",
        );
        write_pill(
            &mut svg,
            W - PAD - 38.0,
            PAD + 12.0,
            kind_badge,
            10.5,
            p.badge_bg,
            p.muted,
            Some(p.border),
        );
        if !subtitle_lines.is_empty() {
            write_text_lines(
                &mut svg,
                &subtitle_lines,
                PAD,
                PAD + title_lines.len() as f32 * 27.0 + 16.0,
                19.0,
                13.5,
                p.muted,
                "400",
                "start",
            );
        }
        let div_y = header_h + 6.0;
        let _ = write!(
            svg,
            r#"<line x1="{PAD:.1}" y1="{div_y:.1}" x2="{:.1}" y2="{div_y:.1}" stroke="{}" stroke-width="1"/>"#,
            W - PAD,
            p.border
        );
        svg.push_str(&body);
        svg.push_str("</g></svg>");
        svg
    }

    fn measure_card(&self, node: &Node, width: f32) -> (Vec<String>, Vec<String>, f32) {
        let inner_w = (width - 32.0).max(120.0);
        let label_lines = wrap_text(&node.label, inner_w, 15.5, 2);
        let detail_lines = node
            .detail
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| wrap_text(s, inner_w, 13.0, 3))
            .unwrap_or_default();
        let has_tag = node.tag.as_deref().is_some_and(|t| !t.trim().is_empty());
        let mut h = 14.0;
        if has_tag {
            h += 22.0;
        }
        h += label_lines.len().max(1) as f32 * 21.0;
        if !detail_lines.is_empty() {
            h += 5.0 + detail_lines.len() as f32 * 18.0;
        }
        h += 12.0;
        (label_lines, detail_lines, h.max(54.0))
    }

    fn draw_card(
        &self,
        svg: &mut String,
        p: Palette,
        node: &Node,
        rect: BoxRect,
        index_badge: Option<usize>,
        force_emphasis: bool,
    ) {
        let emph = force_emphasis || node.emphasis.unwrap_or(false);
        let fill = if emph { p.card_emphasis } else { p.card };
        let stroke = if emph { p.border_emphasis } else { p.border };
        let sw = if emph { "1.6" } else { "1.1" };
        let _ = write!(
            svg,
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="12" fill="{fill}" stroke="{stroke}" stroke-width="{sw}"/>"#,
            rect.x, rect.y, rect.w, rect.h
        );
        let (label_lines, detail_lines, _) = self.measure_card(node, rect.w);
        let mut y = rect.y + 16.0;
        let text_x = rect.x + 16.0;
        if let Some(tag) = node.tag.as_deref().filter(|t| !t.trim().is_empty()) {
            let tw = (estimate_width_px(tag, 11.5) + 14.0).min(rect.w - 32.0);
            let _ = write!(
                svg,
                r#"<rect x="{text_x:.1}" y="{:.1}" width="{tw:.1}" height="19" rx="9.5" fill="{}"/><text x="{:.1}" y="{:.1}" font-size="11.5" font-weight="600" fill="{}">{}</text>"#,
                y - 4.0,
                p.accent_soft,
                text_x + 7.0,
                y + 9.5,
                p.accent,
                image_charts::escape(tag)
            );
            y += 23.0;
        } else if let Some(num) = index_badge {
            let _ = write!(
                svg,
                r#"<circle cx="{:.1}" cy="{:.1}" r="10" fill="{}"/><text x="{:.1}" y="{:.1}" font-size="11.5" font-weight="700" fill="{}" text-anchor="middle">{num}</text>"#,
                rect.right() - 20.0,
                rect.y + 20.0,
                p.accent_soft,
                rect.right() - 20.0,
                rect.y + 24.0,
                p.accent
            );
        }
        write_text_lines(
            svg,
            &label_lines,
            text_x,
            y + 13.0,
            21.0,
            15.5,
            p.ink,
            "600",
            "start",
        );
        if !detail_lines.is_empty() {
            let dy = y + label_lines.len().max(1) as f32 * 21.0 + 16.0;
            write_text_lines(
                svg,
                &detail_lines,
                text_x,
                dy,
                18.0,
                13.0,
                p.muted,
                "400",
                "start",
            );
        }
    }

    fn compute_layers(&self) -> Vec<Vec<usize>> {
        let n = self.nodes.len();
        let mut indeg = vec![0usize; n];
        let mut adj = vec![Vec::new(); n];
        for r in &self.relations {
            adj[r.from].push(r.to);
            indeg[r.to] += 1;
        }
        let mut layer = vec![0usize; n];
        let mut q = VecDeque::new();
        for (i, &d) in indeg.iter().enumerate() {
            if d == 0 {
                q.push_back(i);
            }
        }
        if q.is_empty() {
            q.push_back(0);
        }
        let mut visited = vec![false; n];
        while let Some(u) = q.pop_front() {
            visited[u] = true;
            for &v in &adj[u] {
                if layer[v] <= layer[u] && layer[u] + 1 < n {
                    layer[v] = layer[u] + 1;
                }
                if indeg[v] > 0 {
                    indeg[v] -= 1;
                    if indeg[v] == 0 {
                        q.push_back(v);
                    }
                }
            }
        }
        for i in 0..n {
            if !visited[i] {
                layer[i] = layer.iter().copied().max().unwrap_or(0) + 1;
            }
        }
        let mut grouped: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (i, l) in layer.into_iter().enumerate() {
            grouped.entry(l).or_default().push(i);
        }
        // Keep at most 2 nodes per row for readability.
        let mut rows = Vec::new();
        for (_, nodes) in grouped {
            for chunk in nodes.chunks(2) {
                rows.push(chunk.to_vec());
            }
        }
        rows
    }

    fn render_flow(&self, svg: &mut String, p: Palette, w: f32, pad: f32, top: f32) -> f32 {
        let rows = self.compute_layers();
        let gap_x = 24.0;
        let gap_y = 46.0;
        let inner_w = w - pad * 2.0;
        let mut rects = vec![
            BoxRect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            };
            self.nodes.len()
        ];
        let mut y = top;
        for row in &rows {
            let count = row.len();
            let card_w = if count == 1 {
                (inner_w * 0.78).min(540.0)
            } else {
                (inner_w - gap_x) / 2.0
            };
            let row_h = row
                .iter()
                .map(|&idx| self.measure_card(&self.nodes[idx], card_w).2)
                .fold(54.0f32, f32::max);
            let total_w = card_w * count as f32 + gap_x * (count.saturating_sub(1)) as f32;
            let start_x = (w - total_w) * 0.5;
            for (col, &idx) in row.iter().enumerate() {
                rects[idx] = BoxRect {
                    x: start_x + col as f32 * (card_w + gap_x),
                    y,
                    w: card_w,
                    h: row_h,
                };
            }
            y += row_h + gap_y;
        }
        let bottom = (y - gap_y).max(top + 40.0);

        // Draw edges first so cards sit cleanly on top.
        for r in &self.relations {
            let a = rects[r.from];
            let b = rects[r.to];
            let is_contrast = r.kind == RelationKind::Contrast;
            let stroke = if is_contrast { p.contrast } else { p.accent };
            let marker = if is_contrast {
                "url(#arr-contrast)"
            } else {
                "url(#arr-accent)"
            };
            let dash = if is_contrast {
                r#" stroke-dasharray="5 4""#
            } else {
                ""
            };
            let (d, mid_x, mid_y) = if (a.y - b.y).abs() < 4.0 {
                // Same row: horizontal connector
                let (x1, x2) = if a.cx() < b.cx() {
                    (a.right() + 2.0, b.x - 4.0)
                } else {
                    (a.x - 2.0, b.right() + 4.0)
                };
                let cy = a.cy();
                (
                    format!("M {x1:.1} {cy:.1} L {x2:.1} {cy:.1}"),
                    (x1 + x2) * 0.5,
                    cy - 14.0,
                )
            } else if b.y > a.y {
                let x1 = a.cx();
                let y1 = a.bottom() + 2.0;
                let x2 = b.cx();
                let y2 = b.y - 4.0;
                let ctrl = ((y2 - y1) * 0.45).max(18.0);
                (
                    format!(
                        "M {x1:.1} {y1:.1} C {x1:.1} {:.1}, {x2:.1} {:.1}, {x2:.1} {y2:.1}",
                        y1 + ctrl,
                        y2 - ctrl
                    ),
                    (x1 + x2) * 0.5,
                    (y1 + y2) * 0.5,
                )
            } else {
                // Back-edge: route gently along right margin
                let x1 = a.right() + 2.0;
                let y1 = a.cy();
                let x2 = b.right() + 4.0;
                let y2 = b.cy();
                let arc_x = (a.right().max(b.right()) + 28.0).min(w - 12.0);
                (
                    format!(
                        "M {x1:.1} {y1:.1} C {arc_x:.1} {y1:.1}, {arc_x:.1} {y2:.1}, {x2:.1} {y2:.1}"
                    ),
                    arc_x - 6.0,
                    (y1 + y2) * 0.5,
                )
            };
            let _ = write!(
                svg,
                r#"<path d="{d}" fill="none" stroke="{stroke}" stroke-width="1.8"{dash} marker-end="{marker}"/>"#
            );
            if !r.label.trim().is_empty() {
                let bg = if is_contrast {
                    p.contrast_soft
                } else {
                    p.bg
                };
                write_pill(
                    svg,
                    mid_x,
                    mid_y,
                    r.label.trim(),
                    11.5,
                    bg,
                    stroke,
                    Some(stroke),
                );
            }
        }

        for (i, node) in self.nodes.iter().enumerate() {
            self.draw_card(svg, p, node, rects[i], Some(i + 1), false);
        }
        bottom
    }

    fn render_timeline(&self, svg: &mut String, p: Palette, w: f32, pad: f32, top: f32) -> f32 {
        let axis_x = pad + 112.0;
        let card_x = axis_x + 28.0;
        let card_w = w - pad - card_x;
        let gap_y = 18.0;
        let mut y = top;
        let mut centers = Vec::with_capacity(self.nodes.len());
        let mut rects = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            // Render tag on the left of the axis, so measure card without tag inside
            let stripped = Node {
                label: node.label.clone(),
                detail: node.detail.clone(),
                tag: None,
                value: node.value,
                emphasis: node.emphasis,
                evidence: node.evidence.clone(),
            };
            let (_, _, h) = self.measure_card(&stripped, card_w);
            let rect = BoxRect {
                x: card_x,
                y,
                w: card_w,
                h,
            };
            centers.push(rect.cy());
            rects.push(rect);
            y += h + gap_y;
        }
        if let (Some(&first_y), Some(&last_y)) = (centers.first(), centers.last()) {
            let _ = write!(
                svg,
                r#"<line x1="{axis_x:.1}" y1="{first_y:.1}" x2="{axis_x:.1}" y2="{last_y:.1}" stroke="{}" stroke-width="2.5" stroke-linecap="round"/>"#,
                p.accent_soft
            );
        }
        for (i, node) in self.nodes.iter().enumerate() {
            let cy = centers[i];
            let emph = node.emphasis.unwrap_or(false);
            let tag = node
                .tag
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("#{}", i + 1));
            let tag_lines = wrap_text(&tag, 96.0, 12.5, 2);
            let ty = cy - (tag_lines.len().saturating_sub(1) as f32 * 8.0) + 4.0;
            write_text_lines(
                svg,
                &tag_lines,
                axis_x - 16.0,
                ty,
                16.0,
                12.5,
                p.accent,
                "700",
                "end",
            );
            let _ = write!(
                svg,
                r#"<circle cx="{axis_x:.1}" cy="{cy:.1}" r="7.5" fill="{}" stroke="{}" stroke-width="2.5"/>"#,
                if emph { p.accent } else { p.bg },
                p.accent
            );
            let _ = write!(
                svg,
                r#"<line x1="{:.1}" y1="{cy:.1}" x2="{:.1}" y2="{cy:.1}" stroke="{}" stroke-width="1.4"/>"#,
                axis_x + 8.0,
                card_x - 2.0,
                p.border
            );
            let stripped = Node {
                label: node.label.clone(),
                detail: node.detail.clone(),
                tag: None,
                value: node.value,
                emphasis: node.emphasis,
                evidence: node.evidence.clone(),
            };
            self.draw_card(svg, p, &stripped, rects[i], None, emph);
        }
        (y - gap_y).max(top + 40.0)
    }

    fn render_compare(&self, svg: &mut String, p: Palette, w: f32, pad: f32, top: f32) -> f32 {
        // Group into 2 columns by distinct tags if possible, otherwise split evenly.
        let mut distinct_tags: Vec<String> = Vec::new();
        for n in &self.nodes {
            if let Some(t) = n.tag.as_deref().map(str::trim).filter(|t| !t.is_empty())
                && !distinct_tags.iter().any(|x| x == t)
            {
                distinct_tags.push(t.to_owned());
            }
        }
        let (left_title, right_title, left_idx, right_idx): (String, String, Vec<usize>, Vec<usize>) =
            if distinct_tags.len() == 2 {
                let l = distinct_tags[0].clone();
                let r = distinct_tags[1].clone();
                let li = self
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.tag.as_deref().map(str::trim) == Some(l.as_str()))
                    .map(|(i, _)| i)
                    .collect();
                let ri = self
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.tag.as_deref().map(str::trim) == Some(r.as_str()))
                    .map(|(i, _)| i)
                    .collect();
                (l, r, li, ri)
            } else {
                let mid = (self.nodes.len() + 1) / 2;
                (
                    "A".to_owned(),
                    "B".to_owned(),
                    (0..mid).collect(),
                    (mid..self.nodes.len()).collect(),
                )
            };

        let gap_x = 28.0;
        let col_w = (w - pad * 2.0 - gap_x) * 0.5;
        let left_x = pad;
        let right_x = pad + col_w + gap_x;
        let header_h = 30.0;

        // Column headers
        let _ = write!(
            svg,
            r#"<rect x="{left_x:.1}" y="{top:.1}" width="{col_w:.1}" height="{header_h:.1}" rx="8" fill="{}"/><text x="{:.1}" y="{:.1}" font-size="13.5" font-weight="700" fill="{}" text-anchor="middle">{}</text>"#,
            p.accent_soft,
            left_x + col_w * 0.5,
            top + 20.0,
            p.accent,
            image_charts::escape(&left_title)
        );
        let _ = write!(
            svg,
            r#"<rect x="{right_x:.1}" y="{top:.1}" width="{col_w:.1}" height="{header_h:.1}" rx="8" fill="{}"/><text x="{:.1}" y="{:.1}" font-size="13.5" font-weight="700" fill="{}" text-anchor="middle">{}</text>"#,
            p.contrast_soft,
            right_x + col_w * 0.5,
            top + 20.0,
            p.contrast,
            image_charts::escape(&right_title)
        );

        let start_y = top + header_h + 14.0;
        let gap_y = 14.0;
        let mut ly = start_y;
        for &idx in &left_idx {
            let stripped = Node {
                tag: if distinct_tags.len() == 2 {
                    None
                } else {
                    self.nodes[idx].tag.clone()
                },
                ..Node {
                    label: self.nodes[idx].label.clone(),
                    detail: self.nodes[idx].detail.clone(),
                    tag: None,
                    value: None,
                    emphasis: self.nodes[idx].emphasis,
                    evidence: self.nodes[idx].evidence.clone(),
                }
            };
            let (_, _, h) = self.measure_card(&stripped, col_w);
            self.draw_card(
                svg,
                p,
                &stripped,
                BoxRect {
                    x: left_x,
                    y: ly,
                    w: col_w,
                    h,
                },
                None,
                false,
            );
            ly += h + gap_y;
        }
        let mut ry = start_y;
        for &idx in &right_idx {
            let stripped = Node {
                tag: if distinct_tags.len() == 2 {
                    None
                } else {
                    self.nodes[idx].tag.clone()
                },
                ..Node {
                    label: self.nodes[idx].label.clone(),
                    detail: self.nodes[idx].detail.clone(),
                    tag: None,
                    value: None,
                    emphasis: self.nodes[idx].emphasis,
                    evidence: self.nodes[idx].evidence.clone(),
                }
            };
            let (_, _, h) = self.measure_card(&stripped, col_w);
            self.draw_card(
                svg,
                p,
                &stripped,
                BoxRect {
                    x: right_x,
                    y: ry,
                    w: col_w,
                    h,
                },
                None,
                false,
            );
            ry += h + gap_y;
        }
        let max_y = ly.max(ry) - gap_y;
        let mid_x = w * 0.5;
        let _ = write!(
            svg,
            r#"<line x1="{mid_x:.1}" y1="{top:.1}" x2="{mid_x:.1}" y2="{max_y:.1}" stroke="{}" stroke-width="1" stroke-dasharray="4 4"/>"#,
            p.border
        );
        max_y.max(start_y + 20.0)
    }

    fn render_hierarchy(&self, svg: &mut String, p: Palette, w: f32, pad: f32, top: f32) -> f32 {
        let root_idx = self
            .nodes
            .iter()
            .position(|n| n.emphasis == Some(true))
            .unwrap_or(0);
        let children: Vec<usize> = (0..self.nodes.len()).filter(|&i| i != root_idx).collect();
        let inner_w = w - pad * 2.0;
        let root_w = (inner_w * 0.72).min(520.0);
        let (_, _, root_h) = self.measure_card(&self.nodes[root_idx], root_w);
        let root_rect = BoxRect {
            x: (w - root_w) * 0.5,
            y: top,
            w: root_w,
            h: root_h,
        };
        let cols = if children.len() == 3 { 3 } else { 2 };
        let gap_x = 18.0;
        let gap_y = 20.0;
        let child_w = (inner_w - gap_x * (cols - 1) as f32) / cols as f32;
        let branch_gap = 40.0;
        let mut y = root_rect.bottom() + branch_gap;
        let mut child_rects = Vec::new();
        for row in children.chunks(cols) {
            let row_h = row
                .iter()
                .map(|&i| self.measure_card(&self.nodes[i], child_w).2)
                .fold(54.0f32, f32::max);
            let total_w = child_w * row.len() as f32 + gap_x * (row.len().saturating_sub(1)) as f32;
            let start_x = (w - total_w) * 0.5;
            for (c, &idx) in row.iter().enumerate() {
                child_rects.push((
                    idx,
                    BoxRect {
                        x: start_x + c as f32 * (child_w + gap_x),
                        y,
                        w: child_w,
                        h: row_h,
                    },
                ));
            }
            y += row_h + gap_y;
        }

        // Draw smooth branches from root to first-row children (and all children).
        let trunk_y = root_rect.bottom() + branch_gap * 0.5;
        for &(idx, rect) in &child_rects {
            let _ = write!(
                svg,
                r#"<path d="M {:.1} {:.1} L {:.1} {trunk_y:.1} L {:.1} {trunk_y:.1} L {:.1} {:.1}" fill="none" stroke="{}" stroke-width="1.6"/>"#,
                root_rect.cx(),
                root_rect.bottom() + 1.0,
                root_rect.cx(),
                rect.cx(),
                rect.cx(),
                rect.y - 1.0,
                p.accent
            );
            if let Some(rel) = self
                .relations
                .iter()
                .find(|r| (r.from == root_idx && r.to == idx) || (r.to == root_idx && r.from == idx))
                .filter(|r| !r.label.trim().is_empty())
            {
                write_pill(
                    svg,
                    rect.cx(),
                    trunk_y,
                    rel.label.trim(),
                    11.0,
                    p.bg,
                    p.accent,
                    Some(p.accent),
                );
            }
        }
        self.draw_card(svg, p, &self.nodes[root_idx], root_rect, None, true);
        for (idx, rect) in child_rects {
            self.draw_card(svg, p, &self.nodes[idx], rect, None, false);
        }
        (y - gap_y).max(root_rect.bottom() + 20.0)
    }

    fn render_metric(&self, svg: &mut String, p: Palette, w: f32, pad: f32, top: f32) -> f32 {
        let inner_w = w - pad * 2.0;
        let values: Vec<f64> = self
            .nodes
            .iter()
            .map(|n| n.value.unwrap_or(0.0).max(0.0))
            .collect();
        let max_val = values
            .iter()
            .copied()
            .fold(0.0f64, f64::max)
            .max(1.0);
        let mut y = top;
        for (i, node) in self.nodes.iter().enumerate() {
            let val = values[i];
            let frac = (val / max_val).clamp(0.04, 1.0) as f32;
            let emph = node.emphasis.unwrap_or(false);
            let bar_color = if emph { p.contrast } else { p.accent };
            let tag = node
                .tag
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if val > 0.0 {
                        format!("{val:.1}")
                    } else {
                        String::new()
                    }
                });
            let _ = write!(
                svg,
                r#"<text x="{pad:.1}" y="{:.1}" font-size="14.5" font-weight="600" fill="{}">{}</text>"#,
                y + 15.0,
                p.ink,
                image_charts::escape(&node.label)
            );
            if !tag.is_empty() {
                let _ = write!(
                    svg,
                    r#"<text x="{:.1}" y="{:.1}" font-size="14.5" font-weight="700" fill="{bar_color}" text-anchor="end">{}</text>"#,
                    w - pad,
                    y + 15.0,
                    image_charts::escape(&tag)
                );
            }
            let bar_y = y + 24.0;
            let bar_h = 14.0;
            let _ = write!(
                svg,
                r#"<rect x="{pad:.1}" y="{bar_y:.1}" width="{inner_w:.1}" height="{bar_h:.1}" rx="7" fill="{}"/><rect x="{pad:.1}" y="{bar_y:.1}" width="{:.1}" height="{bar_h:.1}" rx="7" fill="{bar_color}"/>"#,
                p.track,
                (inner_w * frac).max(16.0)
            );
            y = bar_y + bar_h + 10.0;
            if let Some(detail) = node.detail.as_deref().filter(|s| !s.trim().is_empty()) {
                let dlines = wrap_text(detail, inner_w, 12.5, 2);
                write_text_lines(svg, &dlines, pad, y + 10.0, 17.0, 12.5, p.muted, "400", "start");
                y += dlines.len() as f32 * 17.0 + 10.0;
            } else {
                y += 8.0;
            }
        }
        y.max(top + 40.0)
    }

    fn render_summary(&self, svg: &mut String, p: Palette, w: f32, pad: f32, top: f32) -> f32 {
        let card_w = w - pad * 2.0;
        let gap_y = 12.0;
        let mut y = top;
        for (i, node) in self.nodes.iter().enumerate() {
            let emph = node.emphasis.unwrap_or(false);
            let fill = if emph { p.card_emphasis } else { p.card };
            let stroke = if emph { p.border_emphasis } else { p.border };
            let bar_fill = if emph { p.accent } else { p.accent_soft };
            let tag = node.tag.as_deref().map(str::trim).filter(|t| !t.is_empty());
            let tag_reserve = tag.map_or(0.0, |t| (estimate_width_px(t, 11.5) + 28.0).min(160.0));
            let text_x = pad + 44.0;
            let label_w = (card_w - 60.0 - tag_reserve).max(160.0);
            let detail_w = (card_w - 60.0).max(180.0);
            let label_lines = wrap_text(&node.label, label_w, 15.5, 2);
            let detail_lines = node
                .detail
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|s| wrap_text(s, detail_w, 13.0, 3))
                .unwrap_or_default();
            let mut h = 14.0 + label_lines.len().max(1) as f32 * 21.0 + 12.0;
            if !detail_lines.is_empty() {
                h += 5.0 + detail_lines.len() as f32 * 18.0;
            }
            let h = h.max(52.0);
            let _ = write!(
                svg,
                r#"<rect x="{pad:.1}" y="{y:.1}" width="{card_w:.1}" height="{h:.1}" rx="12" fill="{fill}" stroke="{stroke}" stroke-width="1.1"/>"#
            );
            let _ = write!(
                svg,
                r#"<rect x="{:.1}" y="{:.1}" width="3.5" height="{:.1}" rx="1.75" fill="{bar_fill}"/>"#,
                pad + 1.0,
                y + 10.0,
                (h - 20.0).max(12.0)
            );
            let badge_cx = pad + 24.0;
            let badge_cy = y + 24.0;
            let _ = write!(
                svg,
                r#"<circle cx="{badge_cx:.1}" cy="{badge_cy:.1}" r="10.5" fill="{}"/><text x="{badge_cx:.1}" y="{:.1}" font-size="11.5" font-weight="700" fill="{}" text-anchor="middle">{}</text>"#,
                if emph { p.accent } else { p.accent_soft },
                badge_cy + 4.0,
                if emph { p.bg } else { p.accent },
                i + 1
            );
            write_text_lines(
                svg,
                &label_lines,
                text_x,
                y + 28.0,
                21.0,
                15.5,
                p.ink,
                "600",
                "start",
            );
            if let Some(tag) = tag {
                let tw = (estimate_width_px(tag, 11.5) + 16.0).min(148.0);
                let tx = pad + card_w - 14.0 - tw * 0.5;
                write_pill(svg, tx, y + 23.0, tag, 11.5, p.badge_bg, p.muted, Some(p.border));
            }
            if !detail_lines.is_empty() {
                let dy = y + 14.0 + label_lines.len().max(1) as f32 * 21.0 + 16.0;
                write_text_lines(
                    svg,
                    &detail_lines,
                    text_x,
                    dy,
                    18.0,
                    13.0,
                    p.muted,
                    "400",
                    "start",
                );
            }
            y += h + gap_y;
        }
        (y - gap_y).max(top + 40.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{original_source, section, text};
    use super::*;

    #[test]
    fn passage_diagram_switches_without_mutating_source_or_accepting_invented_evidence() {
        let prose = "Walter Camp played sports and later became a coach. He was born in 1859.";
        let original = section(vec![text("p1", prose), text("p2", "Unrelated neighbor.")]);
        let source_range = source(&original.blocks[0]).unwrap().clone();
        let source = original_source(original.clone());
        let derived = SemanticLayoutSource::new(source.clone(), source);
        let mut plan: Diagram = serde_json::from_value(json!({
            "title":"Walter Camp", "nodes":[{"label":"Athlete","evidence":"played sports"},
            {"label":"Coach","evidence":"later became a coach"}], "relations":[
                {"from":0,"to":1,"label":"later","evidence":"played sports and later became a coach"}
            ]
        }))
        .unwrap();
        let (light_svg, dark_svg) = plan.render_pair(prose).unwrap();
        assert!(light_svg.contains("<path "));
        assert!(dark_svg.contains("#1A1D21"));
        plan.relations[0].evidence = "That caused his career.".into();
        assert!(plan.svg(prose).is_none());
        assert!(derived.install_passage_chart(PassageChart {
            source: source_range.clone(),
            input: digest(prose.as_bytes()),
            light_svg,
            dark_svg,
        }));
        let display = derived.parse_section(0).unwrap();
        let Block::Image(image) = &display.blocks[0] else {
            panic!("diagram not shown")
        };
        assert_eq!(image.source.as_ref(), Some(&source_range));
        assert!(derived.resource(&image.href).is_ok());
        assert_eq!(display.blocks[1], original.blocks[1]);
        assert!(derived.set_passage_chart_dark(true));
        let dark_display = derived.parse_section(0).unwrap();
        let Block::Image(dark_image) = &dark_display.blocks[0] else {
            panic!("dark diagram not shown")
        };
        assert_ne!(dark_image.href, image.href);
        assert!(derived.toggle_passage_chart(&source_range));
        assert_eq!(derived.parse_section(0).unwrap().blocks, original.blocks);
        assert!(derived.toggle_passage_chart(&source_range));
        assert!(matches!(
            derived.parse_section(0).unwrap().blocks[0],
            Block::Image(_)
        ));
    }

    #[test]
    fn renders_all_six_diagram_templates_and_decodes_compact_wire() {
        let prose = "In 1990 alpha grew 40% while beta grew 80%. Alpha is fast; beta is accurate. Both stem from core design.";
        for kind in ["flow", "timeline", "compare", "hierarchy", "metric", "summary"] {
            let plan: Diagram = serde_json::from_value(json!({
                "title": format!("Test {kind}"),
                "subtitle": "Summary line",
                "kind": kind,
                "nodes": [
                    {"label": "Alpha", "detail": "Fast path", "tag": "1990 / 40%", "value": 40.0, "emphasis": true, "evidence": "alpha grew 40%"},
                    {"label": "Beta", "detail": "Accurate path", "tag": "80%", "value": 80.0, "evidence": "beta grew 80%"}
                ],
                "relations": [
                    {"from": 0, "to": 1, "label": "vs", "kind": "contrast", "evidence": "Alpha is fast; beta is accurate"}
                ]
            }))
            .unwrap();
            let (light, dark) = plan.render_pair(prose).expect(kind);
            assert!(light.starts_with("<svg ") && light.ends_with("</svg>"));
            assert!(dark.starts_with("<svg ") && dark.ends_with("</svg>"));
        }
        let compact = json!({
            "ti": "要点",
            "sb": "核心总结",
            "k": "sm",
            "nd": [
                {"lb": "Alpha", "dt": "Fast", "ta": "Core", "em": true, "ev": "Alpha is fast"},
                {"lb": "Beta", "ev": "beta is accurate"}
            ]
        });
        let decoded: Diagram = serde_json::from_value(wire::decode(&compact)).unwrap();
        assert_eq!(decoded.kind, DiagramKind::Summary);
        assert!(decoded.render_pair(prose).is_some());
    }
}
