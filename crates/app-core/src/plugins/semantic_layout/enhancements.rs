//! Derived, source-backed presentation generated as part of the layout job.
//! No model calls, resource reads or SVG rendering happen in preview UI code.
use super::*;

mod charts;
const PROMPT: &str = include_str!("enhancements/prompt.md");
pub(super) const REVISION: u32 = 1;
pub(super) fn contract() -> Value {
    static CONTRACT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    CONTRACT
        .get_or_init(|| {
            json!([
                REVISION,
                digest(PROMPT.as_bytes()),
                digest(schema().to_string().as_bytes())
            ])
        })
        .clone()
}

// A standalone formula can carry an authored equation label, e.g. “(3.1)”.
// Accept only a compact parenthesized number, never adjacent prose.
fn standalone_formula(text: &TextBlock) -> bool {
    let mut label = String::new();
    for inline in &text.content {
        match inline {
            Inline::Text(run) => label.push_str(&run.text),
            Inline::Math(_) | Inline::Image(_) | Inline::Break => {}
            _ => return false,
        }
    }
    let label = label.trim();
    if label.is_empty() {
        return true;
    }
    let inner = label
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .or_else(|| label.strip_prefix('（').and_then(|s| s.strip_suffix('）')));
    inner.is_some_and(|s| {
        s.len() <= 24
            && s.chars().any(|c| c.is_ascii_digit())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ' '))
    })
}

pub fn display_formula(block: &Block) -> Option<(SourceRange, String)> {
    match block {
        Block::Image(image) => Some((image.source.clone()?, image.formula.as_ref()?.latex.clone())),
        Block::Text(text) => {
            let standalone = standalone_formula(text);
            text.content.iter().find_map(|i| match i {
                Inline::Math(m) if standalone || m.display => {
                    Some((text.source.clone()?, m.latex.clone()))
                }
                Inline::Image(run) if standalone => Some((
                    text.source.clone()?,
                    run.image.formula.as_ref()?.latex.clone(),
                )),
                _ => None,
            })
        }
        _ => None,
    }
}

pub(super) fn chart_href(svg: &str) -> PublicationUrl {
    PublicationUrl::parse(&format!("torto-chart/{}.svg", digest(svg.as_bytes())))
        .expect("generated chart URL")
}

pub(super) fn apply_chart(
    blocks: &mut [Block],
    root: &SourceRange,
    members: &[SourceRange],
    svg: &str,
) {
    for block in blocks {
        match block {
            Block::Table(table)
                if table
                    .source
                    .as_ref()
                    .is_some_and(|s| members.iter().any(|m| same(s, m))) =>
            {
                table.visualization = Some(rebook_publication::TableVisualization {
                    root: root.clone(),
                    href: table
                        .source
                        .as_ref()
                        .filter(|s| same(s, root))
                        .map(|_| chart_href(svg)),
                    active: false,
                });
            }
            Block::Figure(f) => apply_chart(&mut f.body, root, members, svg),
            Block::Note(n) => apply_chart(&mut n.blocks, root, members, svg),
            _ => {}
        }
    }
}
fn same(a: &SourceRange, b: &SourceRange) -> bool {
    a.start.spine == b.start.spine && a.start.node == b.start.node
}

impl SemanticLayoutSource {
    pub fn toggle_formula_notes(&self, sources: &[SourceRange]) -> bool {
        if self.formula_notes_for_sources(sources).is_empty() {
            return false;
        }
        let Ok(mut views) = self.formula_note_views.try_write() else {
            return false;
        };
        let keys = sources
            .iter()
            .map(|source| format!("{:?}#{}", source.start.spine, source.start.node))
            .collect::<Vec<_>>();
        let visible = keys.iter().any(|key| views.contains(key));
        if !visible && views.len() + keys.len() > 128 {
            views.clear();
        }
        for key in keys {
            if visible {
                views.remove(&key);
            } else {
                views.insert(key);
            }
        }
        true
    }
    fn with_derived<T>(&self, mut f: impl FnMut(&Annotation) -> Option<T>) -> Option<T> {
        if !self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        if let Ok(state) = self.state.try_read() {
            for recognition in state.values() {
                for a in &recognition.annotations {
                    if let Some(v) = f(a) {
                        return Some(v);
                    }
                }
            }
        }
        if let Ok(partial) = self.partial.try_read() {
            for groups in partial.values() {
                for recognition in groups.values() {
                    for a in &recognition.annotations {
                        if let Some(v) = f(a) {
                            return Some(v);
                        }
                    }
                }
            }
        }
        None
    }
    pub fn formula_notes(&self, source: &SourceRange, latex: &str) -> Vec<SymbolNote> {
        self.with_derived(|a| match a {
            Annotation::FormulaNotes {
                source: s,
                latex: l,
                notes,
                ..
            } if same(s, source) && l == latex => Some(notes.clone()),
            _ => None,
        })
        .unwrap_or_default()
    }
    pub fn chart_control(&self, source: &SourceRange) -> Option<(SourceRange, bool)> {
        let root = self.with_derived(|a| match a {
            Annotation::TableChart {
                source: root,
                members,
                ..
            } if members.iter().any(|m| same(m, source)) => Some(root.clone()),
            _ => None,
        })?;
        let active = self
            .chart_views
            .try_read()
            .ok()?
            .contains(&format!("{:?}#{}", root.start.spine, root.start.node));
        Some((root, active))
    }

    pub fn formula_notes_for_sources(&self, sources: &[SourceRange]) -> Vec<SymbolNote> {
        if !self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut collect = |annotation: &Annotation| {
            if let Annotation::FormulaNotes { source, notes, .. } = annotation
                && sources.iter().any(|candidate| same(source, candidate))
            {
                for note in notes {
                    if seen.insert((note.symbol.clone(), note.meaning.clone(), note.unit.clone())) {
                        result.push(note.clone());
                    }
                }
            }
        };
        if let Ok(state) = self.state.try_read() {
            for recognition in state.values() {
                for annotation in &recognition.annotations {
                    collect(annotation);
                }
            }
        }
        if let Ok(partial) = self.partial.try_read() {
            for recognition in partial.values().flat_map(|groups| groups.values()) {
                for annotation in &recognition.annotations {
                    collect(annotation);
                }
            }
        }
        result
    }

    pub fn chart_anchor(&self, origin: &SourceRange) -> Option<SourceRange> {
        let (root, active) = self.chart_control(origin)?;
        if active {
            return Some(root);
        }
        self.with_derived(|a| match a {
            Annotation::TableChart {
                source, members, ..
            } if same(source, &root) => members.iter().find(|m| !same(m, source)).cloned(),
            _ => None,
        })
    }
    pub fn toggle_chart(&self, source: &SourceRange) -> bool {
        let Some((root, _)) = self.chart_control(source) else {
            return false;
        };
        let Ok(mut views) = self.chart_views.try_write() else {
            return false;
        };
        let key = format!("{:?}#{}", root.start.spine, root.start.node);
        if !views.remove(&key) {
            if views.len() >= 128 {
                views.clear()
            }
            views.insert(key);
        }
        true
    }
    pub(super) fn chart_resource(&self, href: &PublicationUrl) -> Option<Resource> {
        if !href.path().starts_with("torto-chart/") {
            return None;
        }
        self.with_derived(|a| match a {
            Annotation::TableChart { svg, .. } if chart_href(svg) == *href => Some(Resource {
                href: href.clone(),
                media_type: "image/svg+xml".into(),
                bytes: Arc::from(svg.as_bytes()),
            }),
            _ => None,
        })
    }
    pub(super) fn chart_raster(
        &self,
        href: &PublicationUrl,
    ) -> Option<Result<RasterResource, PublicationError>> {
        self.chart_resource(href)
            .map(|resource| charts::raster(&resource.bytes, &self.chart_fonts))
    }
    pub(super) fn apply_chart_views(&self, blocks: &mut [Block]) {
        fn visit(source: &SemanticLayoutSource, blocks: &mut [Block]) {
            for b in blocks {
                match b {
                    Block::Table(t) => {
                        if let (Some(origin), Some(v)) = (&t.source, &mut t.visualization) {
                            v.active = source
                                .chart_control(origin)
                                .is_some_and(|(_, active)| active)
                        }
                    }
                    Block::Figure(f) => visit(source, &mut f.body),
                    Block::Note(n) => visit(source, &mut n.blocks),
                    _ => {}
                }
            }
        }
        visit(self, blocks)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SymbolNote {
    pub symbol: String,
    pub meaning: String,
    pub unit: String,
    pub evidence: String,
    pub known: bool,
}
impl SymbolNote {
    pub fn render_note(&self) -> rebook_publication::FormulaNote {
        rebook_publication::FormulaNote {
            symbol: self.symbol.clone(),
            meaning: self.meaning.clone(),
            unit: self.unit.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Series {
    name: String,
    cells: Vec<[usize; 3]>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Panel {
    title: String,
    unit: String,
    categories: Vec<String>,
    series: Vec<Series>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Chart {
    tables: Vec<usize>,
    kind: String,
    title: String,
    panels: Vec<Panel>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Notes {
    id: usize,
    notes: Vec<SymbolNote>,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Output {
    charts: Vec<Chart>,
    notes: Vec<Notes>,
}

struct Table<'a> {
    source: SourceRange,
    table: &'a rebook_publication::TableBlock,
    block: usize,
    context: String,
}
struct Formula {
    source: SourceRange,
    latex: String,
    context: String,
}

fn context(section: &Section, block: usize) -> String {
    section.blocks[block.saturating_sub(3)..(block + 5).min(section.blocks.len())]
        .iter()
        .flat_map(Block::text_blocks)
        .map(text_block_text)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(7000)
        .collect()
}

pub(super) fn has_candidates(section: &Section) -> bool {
    let (t, f) = candidates(section);
    !t.is_empty() || !f.is_empty()
}

fn candidates(section: &Section) -> (Vec<Table<'_>>, Vec<Formula>) {
    fn visit<'a>(
        blocks: &'a [Block],
        at: usize,
        cx: &str,
        tables: &mut Vec<Table<'a>>,
        formulas: &mut Vec<Formula>,
    ) {
        for block in blocks {
            match block {
                Block::Table(table) => {
                    if let Some(source) = &table.source
                        && tables.len() < 12
                        && table.rows.len() <= 256
                        && table
                            .text_blocks()
                            .map(|t| text_block_text(t).len())
                            .sum::<usize>()
                            <= 24000
                    {
                        tables.push(Table {
                            source: source.clone(),
                            table,
                            block: at,
                            context: cx.into(),
                        });
                    }
                }
                Block::Text(text) => {
                    let standalone = standalone_formula(text);
                    for inline in &text.content {
                        if let Inline::Math(math) = inline
                            && (math.display || standalone)
                            && let Some(source) = &text.source
                            && formulas.len() < 32
                        {
                            formulas.push(Formula {
                                source: source.clone(),
                                latex: math.latex.clone(),
                                context: cx.into(),
                            });
                        }
                        if standalone
                            && let Inline::Image(run) = inline
                            && let (Some(formula), Some(source)) =
                                (&run.image.formula, &text.source)
                            && formulas.len() < 32
                        {
                            formulas.push(Formula {
                                source: source.clone(),
                                latex: formula.latex.clone(),
                                context: cx.into(),
                            });
                        }
                    }
                }
                Block::Image(image) => {
                    if let (Some(formula), Some(source)) = (&image.formula, &image.source)
                        && formulas.len() < 32
                    {
                        formulas.push(Formula {
                            source: source.clone(),
                            latex: formula.latex.clone(),
                            context: cx.into(),
                        });
                    }
                }
                Block::Figure(figure) => {
                    visit(&figure.body, at, cx, tables, formulas);
                    for image in &figure.images {
                        if let (Some(formula), Some(source)) = (&image.formula, &image.source)
                            && formulas.len() < 32
                        {
                            formulas.push(Formula {
                                source: source.clone(),
                                latex: formula.latex.clone(),
                                context: cx.into(),
                            });
                        }
                    }
                }
                Block::Note(note) => visit(&note.blocks, at, cx, tables, formulas),
                Block::Quote(quote) => {
                    for text in &quote.body {
                        visit(
                            std::slice::from_ref(&Block::Text(text.clone())),
                            at,
                            cx,
                            &mut Vec::new(),
                            formulas,
                        );
                    }
                }
                _ => {}
            }
        }
    }
    let (mut tables, mut formulas) = (Vec::new(), Vec::new());
    for (at, block) in section.blocks.iter().enumerate() {
        visit(
            std::slice::from_ref(block),
            at,
            &context(section, at),
            &mut tables,
            &mut formulas,
        );
    }
    (tables, formulas)
}

fn schema() -> Value {
    let string = json!({"type":"string"});
    let note = json!({"type":"object","additionalProperties":false,"properties":{
        "symbol":string,"meaning":string,"unit":string,"evidence":string,"known":{"type":"boolean"}},
        "required":["symbol","meaning","unit","evidence","known"]});
    let series = json!({"type":"object","additionalProperties":false,"properties":{
        "name":string,"cells":{"type":"array","items":{"type":"array","items":{"type":"integer"},"minItems":3,"maxItems":3}}},"required":["name","cells"]});
    let panel = json!({"type":"object","additionalProperties":false,"properties":{
        "title":string,"unit":string,"categories":{"type":"array","items":string},"series":{"type":"array","items":series}},"required":["title","unit","categories","series"]});
    json!({"temperature":0.0,"output_schema":{"type":"object","additionalProperties":false,"properties":{
        "charts":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{
            "tables":{"type":"array","items":{"type":"integer"}},"kind":{"type":"string","enum":["bar","line","pie"]},"title":string,
            "panels":{"type":"array","items":panel}},"required":["tables","kind","title","panels"]}},
        "notes":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{
            "id":{"type":"integer"},"notes":{"type":"array","items":note}},"required":["id","notes"]}}},"required":["charts","notes"]}})
}

fn input(tables: &[Table<'_>], formulas: &[Formula], language: &str) -> Value {
    json!({"target_language":language,
        "tables":tables.iter().enumerate().map(|(id,t)|json!({"id":id,"context":t.context,
            "rows":t.table.rows.iter().map(|r|r.cells.iter().map(|c|json!({"text":text_block_text(&c.text),"row_span":c.row_span,"column_span":c.column_span,"header":c.header})).collect::<Vec<_>>()).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "formulas":formulas.iter().enumerate().map(|(id,f)|json!({"id":id,"latex":f.latex,"context":f.context})).collect::<Vec<_>>()})
}

fn numeric(text: &str) -> Option<f64> {
    let text = text.trim();
    let text = text
        .strip_suffix('%')
        .or_else(|| text.strip_suffix('％'))
        .unwrap_or(text)
        .trim();
    let mut value = text.replace(['−', '–', '﹣', '－'], "-").replace('，', ",");
    if value.contains(',') {
        let (whole, fraction) = value
            .split_once('.')
            .map_or((value.as_str(), None), |(a, b)| (a, Some(b)));
        let unsigned = whole.trim_start_matches(['+', '-']);
        let groups = unsigned.split(',').collect::<Vec<_>>();
        if groups.len() < 2
            || groups[0].is_empty()
            || groups[0].len() > 3
            || groups
                .iter()
                .any(|g| !g.bytes().all(|c| c.is_ascii_digit()))
            || groups[1..].iter().any(|g| g.len() != 3)
            || fraction.is_some_and(|f| f.contains(','))
            || groups.len() == 2 && fraction.is_none()
        {
            return None;
        }
        value = value.replace(',', "");
    }
    let value: f64 = value.parse().ok()?;
    (value.is_finite() && value.abs() <= 1e18).then_some(value)
}
fn normalized_symbol(text: &str) -> String {
    text.replace("\\left", "")
        .replace("\\right", "")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '{' && *c != '}')
        .collect()
}
fn short(text: &str, limit: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= limit
        && !text.chars().any(|c| c.is_control() && c != '\n')
}

fn validate(
    output: Output,
    tables: &[Table<'_>],
    formulas: &[Formula],
    language: &str,
) -> Vec<Annotation> {
    let mut annotations = Vec::new();
    let mut used: HashSet<usize> = HashSet::new();
    for chart in output.charts.into_iter().take(12) {
        if chart.tables.is_empty()
            || chart.tables.len() > 4
            || !short(&chart.title, 160)
            || !matches!(chart.kind.as_str(), "bar" | "line" | "pie")
            || chart.panels.is_empty()
            || chart.panels.len() > 4
            || chart
                .tables
                .iter()
                .any(|id| *id >= tables.len() || used.contains(id))
        {
            continue;
        }
        if chart
            .tables
            .windows(2)
            .any(|ids| ids[1] != ids[0] + 1 || tables[ids[1]].block > tables[ids[0]].block + 2)
        {
            continue;
        }
        if chart.tables.len() > 1
            && chart.tables.iter().skip(1).any(|id| {
                let text = tables[*id].context.to_lowercase();
                !["续表", "continued", "continuation"]
                    .iter()
                    .any(|marker| text.contains(marker))
                    || tables[*id].table.rows.first().map(|r| r.cells.len())
                        != tables[chart.tables[0]]
                            .table
                            .rows
                            .first()
                            .map(|r| r.cells.len())
            })
        {
            continue;
        }
        let mut panels = Vec::new();
        for panel in &chart.panels {
            if !short(&panel.title, 160)
                || panel.unit.len() > 100
                || panel.categories.len() < 2
                || panel.categories.len() > 64
                || panel.categories.iter().any(|v| !short(v, 100))
                || panel.series.is_empty()
                || panel.series.len() > 6
            {
                break;
            }
            let mut series = Vec::new();
            for value in &panel.series {
                if !short(&value.name, 100) || value.cells.len() != panel.categories.len() {
                    break;
                }
                let values: Option<Vec<_>> = value
                    .cells
                    .iter()
                    .map(|[t, r, c]| {
                        if !chart.tables.contains(t) {
                            return None;
                        }
                        numeric(&text_block_text(
                            &tables.get(*t)?.table.rows.get(*r)?.cells.get(*c)?.text,
                        ))
                    })
                    .collect();
                let Some(values) = values else { break };
                if chart.kind == "pie"
                    && (panel.series.len() != 1
                        || panel.categories.len() > 12
                        || values.iter().any(|v| *v < 0.0)
                        || values.iter().sum::<f64>() <= 0.0)
                {
                    break;
                }
                series.push((value.name.clone(), values));
            }
            if series.len() != panel.series.len() {
                break;
            }
            panels.push(charts::Panel {
                title: panel.title.clone(),
                unit: panel.unit.clone(),
                categories: panel.categories.clone(),
                series,
            });
        }
        if panels.len() != chart.panels.len() {
            continue;
        }
        let svg = charts::render(&chart.kind, &chart.title, &panels);
        let mut members = vec![tables[chart.tables[0]].source.clone()];
        members.extend(
            chart
                .tables
                .iter()
                .flat_map(|i| tables[*i].table.text_blocks())
                .filter_map(|t| t.source.clone()),
        );
        members.extend(
            chart
                .tables
                .iter()
                .skip(1)
                .map(|i| tables[*i].source.clone()),
        );
        used.extend(chart.tables.iter().copied());
        annotations.push(Annotation::TableChart {
            source: members[0].clone(),
            members,
            svg,
            language: language.into(),
        });
    }
    let mut used = HashSet::new();
    for result in output.notes.into_iter().take(32) {
        let Some(formula) = formulas.get(result.id) else {
            continue;
        };
        if !used.insert(result.id) {
            continue;
        }
        let mut symbols = HashSet::new();
        let notes = result
            .notes
            .into_iter()
            .take(24)
            .filter(|note| {
                short(&note.symbol, 120)
                    && short(&note.meaning, 600)
                    && note.unit.len() <= 100
                    && note.evidence.len() <= 1500
                    && normalized_symbol(&formula.latex).contains(&normalized_symbol(&note.symbol))
                    && (!note.known
                        || !note.evidence.trim().is_empty()
                            && formula.context.contains(&note.evidence))
                    && (note.known || note.evidence.is_empty() && note.unit.is_empty())
                    && symbols.insert(note.symbol.clone())
            })
            .collect::<Vec<_>>();
        if !notes.is_empty() {
            annotations.push(Annotation::FormulaNotes {
                source: formula.source.clone(),
                latex: formula.latex.clone(),
                notes,
                language: language.into(),
            });
        }
    }
    annotations
}

pub(super) async fn recognize(
    client: &reqwest::Client,
    settings: &PluginSettings,
    section: &Section,
    targets: &[SourceRange],
    book_id: &str,
) -> (Vec<Annotation>, bool) {
    let (mut tables, mut formulas) = candidates(section);
    let included = |source: &SourceRange| targets.iter().any(|target| same(source, target));
    let table_included = |t: &Table<'_>| {
        included(&t.source)
            || t.table
                .text_blocks()
                .filter_map(|t| t.source.as_ref())
                .any(included)
    };
    let primary = tables
        .iter()
        .filter(|t| table_included(t))
        .map(|t| t.block)
        .collect::<Vec<_>>();
    tables.retain(|t| {
        table_included(t)
            || primary.iter().any(|b| b.abs_diff(t.block) <= 1)
                && ["续表", "continued", "continuation"]
                    .iter()
                    .any(|w| t.context.to_lowercase().contains(w))
    });
    formulas.retain(|f| included(&f.source));
    if tables.is_empty() && formulas.is_empty() {
        return (Vec::new(), true);
    }
    let language = settings
        .resolved_target_language(crate::preferences::AppLanguage::system_translation_target());
    let input = input(&tables, &formulas, &language);
    if input.to_string().len() > 192 * 1024 {
        return (Vec::new(), false);
    }
    let Ok((provider, model)) = settings.semantic_layout_endpoint() else {
        return (Vec::new(), false);
    };
    let key = digest(
        &serde_json::to_vec(&json!([
            book_id,
            REVISION,
            PROMPT,
            provider.id,
            provider.base_url,
            model,
            settings.semantic_layout.reasoning_effort,
            input
        ]))
        .unwrap(),
    );
    let path = cache_path(&format!("enhancements-{key}"));
    let cached = path
        .as_ref()
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= 512 * 1024))
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Output>(&b).ok());
    let output = if let Some(output) = cached {
        output
    } else {
        let messages = vec![
            json!({"role":"system","content":wire::instructions(PROMPT,std::iter::once(&input))}),
            json!({"role":"user","content":wire::encode(&input).to_string()}),
        ];
        let options = wire::options(&schema());
        let response = crate::plugins::llm::budgeted(ai::request_completion(
            client,
            provider,
            model,
            &messages,
            None,
            Some(8192),
            settings.semantic_layout.reasoning_effort,
            Some(&options),
        ))
        .await;
        let Ok(response) = response else {
            return (Vec::new(), false);
        };
        let Some(content) = ai::message_content(&response) else {
            return (Vec::new(), false);
        };
        let Ok(output) = llm_json::parse::<Value>(&content).and_then(|v| {
            serde_json::from_value::<Output>(wire::decode(&v)).map_err(|e| e.to_string())
        }) else {
            return (Vec::new(), false);
        };
        output
    };
    let expected =
        output.charts.len() + output.notes.iter().filter(|n| !n.notes.is_empty()).count();
    let annotations = validate(output.clone(), &tables, &formulas, &language);
    let valid = annotations.len() == expected;
    if valid && let Some(path) = path {
        let _ = crate::persistence::write_json_atomic(&path, &output);
    }
    (annotations, valid)
}

#[cfg(test)]
mod tests;
