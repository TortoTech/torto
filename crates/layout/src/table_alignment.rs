//! Infer defaults from body columns after wrapping at the final widths.
use super::{
    Alignment, AlignmentOptions, Arc, PreparedTableCell, PreparedText, TableCell, TextBlockKind,
    edge_content_lines,
};
use rebook_publication::{Inline, InlineRole};

fn structural_cell(cell: &TableCell) -> bool {
    header(cell)
        || cell.column_span > 1
        || cell.row_span > 1
        || cell.text.kind == TextBlockKind::Caption
}

fn header(cell: &TableCell) -> bool {
    cell.header || cell.text.style.keep_table_alignment
}

fn identifier_header(cell: &TableCell) -> bool {
    let value: String = cell
        .text
        .content
        .iter()
        .flat_map(Inline::text_runs)
        .flat_map(|run| run.text.chars())
        .take(65)
        .collect();
    let label = value
        .trim()
        .trim_end_matches(['.', ':', '：'])
        .to_lowercase();
    matches!(
        label.as_str(),
        "#" | "id"
            | "no"
            | "number"
            | "index"
            | "code"
            | "item no"
            | "item number"
            | "model"
            | "model no"
            | "model number"
            | "serial no"
            | "serial number"
            | "part number"
            | "sku"
            | "isbn"
            | "issn"
            | "year"
            | "zip code"
            | "编号"
            | "編號"
            | "序号"
            | "序號"
            | "型号"
            | "型號"
            | "代码"
            | "代碼"
            | "年份"
            | "年度"
            | "邮编"
            | "郵編"
    )
}

pub(super) fn allows_default(cell: &TableCell) -> bool {
    !structural_cell(cell)
        && cell.authored_alignment.is_none()
        && cell.text.style.authored_alignment.is_none()
}

#[derive(Clone, Copy)]
enum Value {
    Empty,
    Missing,
    Number,
    Text { short: bool },
    Other,
}

fn value(cell: &TableCell) -> Value {
    let mut sample = String::new();
    let mut long = false;
    let mut letter = false;
    for inline in &cell.text.content {
        match inline {
            Inline::Text(run) if run.style.inline_role == InlineRole::PageMarker => {}
            Inline::Text(run) if run.style.inline_role == InlineRole::Normal => {
                letter |= run.text.chars().any(char::is_alphabetic);
                if !long {
                    for ch in run.text.chars().filter(|&ch| ch != '\u{2060}') {
                        if sample.len() >= 96 {
                            long = true;
                            break;
                        }
                        sample.push(ch);
                    }
                }
            }
            Inline::Break => sample.push('\n'),
            // Image and formula columns are not prose or comparable scalars.
            _ => return Value::Other,
        }
    }
    let s = sample.trim();
    if s.is_empty() && !long {
        return Value::Empty;
    }
    if !long && matches!(s, "-" | "−" | "–" | "—" | "…" | "N/A" | "n/a") {
        return Value::Missing;
    }
    // A bilingual cell can carry the same scalar on two independent lines.
    if !long
        && s.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .all(numeric)
    {
        return Value::Number;
    }
    if letter {
        Value::Text {
            short: !long && s.chars().count() <= 18,
        }
    } else {
        Value::Other
    }
}

fn trim_sign(s: &str) -> &str {
    s.strip_prefix(['+', '-', '−', '＋', '－'])
        .map_or(s, str::trim_start)
}

fn numeric(s: &str) -> bool {
    fn ascii(ch: char) -> char {
        if matches!(ch, '０'..='９') {
            char::from_u32('0' as u32 + ch as u32 - '０' as u32).unwrap_or(ch)
        } else {
            match ch {
                '，' => ',',
                '．' => '.',
                '−' | '－' => '-',
                '＋' => '+',
                c => c,
            }
        }
    }
    let (s, parenthesized) = if let Some(inner) = s
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .or_else(|| s.strip_prefix('（').and_then(|s| s.strip_suffix('）')))
    {
        (inner.trim(), true)
    } else {
        (s, false)
    };
    let unsigned = trim_sign(s);
    let currency = |ch: char| matches!(ch, '$' | '¥' | '￥' | '€' | '£' | '₹' | '₩');
    let s = if let Some(rest) = unsigned.strip_prefix(currency) {
        let rest = rest.trim_start();
        if !parenthesized && unsigned == s {
            trim_sign(rest)
        } else {
            rest
        }
    } else {
        unsigned
    };
    // Count only a small closed set of units; arbitrary suffixes are often IDs.
    let s = [
        "kWh", "kHz", "MHz", "Hz", "kg", "km", "cm", "mm", "ms", "dB", "°C", "亿元", "万元",
        "美元", "元", "万", "亿", "%", "％", "‰",
    ]
    .iter()
    .find_map(|unit| s.strip_suffix(unit))
    .unwrap_or(s)
    .trim_end();
    let normalized: String = s.chars().map(ascii).collect();
    let mut parts = normalized.split(['e', 'E']);
    let Some(mantissa) = parts.next() else {
        return false;
    };
    if let Some(exponent) = parts.next()
        && (!digits(exponent.strip_prefix(['+', '-']).unwrap_or(exponent))
            || parts.next().is_some())
    {
        return false;
    }
    decimal(mantissa)
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn whole(s: &str, separator: Option<char>) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut groups = s.split(|ch: char| {
        matches!(ch, ' ' | '\u{a0}' | '\u{202f}' | '\'' | '’') || separator == Some(ch)
    });
    let first = groups.next().unwrap_or_default();
    let mut grouped = false;
    for group in groups {
        grouped = true;
        if group.len() != 3 || !digits(group) {
            return false;
        }
    }
    digits(first) && (!grouped || first.len() <= 3)
}

fn decimal(s: &str) -> bool {
    let dot = s.rfind('.');
    let comma = s.rfind(',');
    match (dot, comma) {
        (Some(dot), Some(comma)) => {
            let (decimal, separator) = if dot > comma {
                (dot, ',')
            } else {
                (comma, '.')
            };
            whole(&s[..decimal], Some(separator)) && digits(&s[decimal + 1..])
        }
        (Some(at), None) | (None, Some(at)) => {
            let mark = s.as_bytes()[at] as char;
            if s[..at].contains(mark) {
                whole(s, Some(mark))
            } else {
                (at == 0 || whole(&s[..at], None)) && digits(&s[at + 1..])
            }
        }
        (None, None) => whole(s, None),
    }
}

#[derive(Default)]
struct Column {
    observed: usize,
    number: usize,
    text: usize,
    labels: usize,
    wraps: bool,
    identifier: bool,
}

impl Column {
    fn data(&self) -> bool {
        !self.identifier && self.number >= 2 && self.number * 5 >= self.observed * 4
    }
    fn prose(&self) -> bool {
        self.wraps && self.text > self.observed / 2
    }
    fn labels(&self) -> bool {
        self.labels >= 2 && self.labels * 5 >= self.observed * 4
    }
}

pub(super) struct Columns {
    columns: Vec<Column>,
    pending: Vec<(usize, usize, Value)>,
}

#[cfg(test)]
mod tests {
    use super::{Column, numeric};

    #[test]
    fn identifies_numeric_values_without_treating_identifiers_as_data() {
        for value in [
            "−12.5%",
            "¥ 1,000",
            "1.234,56",
            "1\u{202f}200",
            "(3.17)",
            "3.2e−8",
            "２０元",
            "0.4 kg",
        ] {
            assert!(numeric(value), "{value}");
        }
        for value in [
            "1953–1977",
            "2026-11-16",
            "Q3",
            "IPv6",
            "12/3",
            "第 2 章",
            "3e++6",
            "2.3.4",
            "42 unknown",
        ] {
            assert!(!numeric(value), "{value}");
        }
    }

    #[test]
    fn an_identifier_column_does_not_become_a_data_column() {
        let mut column = Column {
            observed: 5,
            number: 5,
            ..Default::default()
        };
        assert!(column.data());
        column.identifier = true;
        assert!(!column.data());
    }
}

impl Columns {
    pub(super) fn new(count: usize) -> Self {
        Self {
            columns: (0..count).map(|_| Column::default()).collect(),
            pending: Vec::new(),
        }
    }

    pub(super) fn observe(
        &mut self,
        index: usize,
        column: usize,
        cell: &TableCell,
        text: &PreparedText,
    ) {
        if header(cell) && cell.column_span == 1 && identifier_header(cell) {
            self.columns[column].identifier = true;
        }
        if structural_cell(cell) {
            return;
        }
        let value = value(cell);
        let stats = &mut self.columns[column];
        match value {
            Value::Empty | Value::Missing => {}
            Value::Number => {
                stats.observed += 1;
                stats.number += 1;
            }
            Value::Text { short } => {
                stats.observed += 1;
                stats.text += 1;
                stats.labels += usize::from(short);
                stats.wraps |= !stats.wraps && edge_content_lines(text).len() > 1;
            }
            Value::Other => stats.observed += 1,
        }
        if allows_default(cell) && !matches!(value, Value::Empty | Value::Other) {
            self.pending.push((index, column, value));
        }
    }

    pub(super) fn apply(self, cells: &mut [PreparedTableCell]) {
        // Description tables often begin with a narrow column of row labels
        // that fit on one line even when their companion descriptions don't.
        let label_column = self.columns.first().is_some_and(Column::labels)
            && self.columns.iter().skip(1).any(Column::prose);
        for (index, column, value) in self.pending {
            let stats = &self.columns[column];
            let alignment = if stats.data() && matches!(value, Value::Number | Value::Missing) {
                Alignment::Right
            } else if stats.prose() || column == 0 && label_column {
                Alignment::Start
            } else {
                continue;
            };
            // Only recalculate line offsets, keeping the existing wraps,
            // shaping and table width solution.
            Arc::make_mut(&mut cells[index].text.layout)
                .align_table_column(alignment, AlignmentOptions::default());
        }
    }
}
