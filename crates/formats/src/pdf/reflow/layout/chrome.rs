//! Single-occurrence running heads inherit a proven marginal layout only when
//! the printed page number and current outline context also agree.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CounterSide {
    Start,
    End,
}

struct Observation {
    page: usize,
    key: String,
    top: bool,
    y: f64,
    anchor: f64,
    font: f64,
    side: Option<CounterSide>,
    offset: Option<i64>,
    label: String,
    confirmed: bool,
}

#[derive(Default)]
pub(crate) struct Detector {
    observations: BTreeMap<usize, Vec<Observation>>,
    occurrences: HashMap<String, Vec<usize>>,
    fonts: HashMap<i64, usize>,
}

fn label_key(label: &str) -> String {
    let normalize = |text: &str| {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    if let Some((ordinal, title)) = crate::source::split_heading_label(label) {
        format!("{ordinal}:{}", normalize(title))
    } else {
        normalize(label)
    }
}

fn outer_counter(raw: &str) -> (Option<CounterSide>, Option<u32>, String) {
    let words = raw.split_whitespace().collect::<Vec<_>>();
    let number = |word: &str| {
        (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
            .then(|| word.parse::<u32>().ok())
            .flatten()
            .filter(|n| *n > 0)
    };
    if let Some(n) = words.last().and_then(|word| number(word)) {
        (
            Some(CounterSide::End),
            Some(n),
            words[..words.len() - 1].join(" "),
        )
    } else if let Some(n) = words.first().and_then(|word| number(word)) {
        (Some(CounterSide::Start), Some(n), words[1..].join(" "))
    } else {
        (None, None, raw.to_owned())
    }
}

impl Detector {
    pub(crate) fn observe(&mut self, page: &NativePage, index: usize) {
        for glyph in &page.glyphs {
            if glyph.size.is_finite() && glyph.size > 0.0 {
                *self
                    .fonts
                    .entry((glyph.size * 10.0).round() as i64)
                    .or_default() += 1;
            }
        }
        let mut seen = HashSet::new();
        for line in edge_lines(page, None) {
            let Some(key) = edge_key(&line, page.height) else {
                continue;
            };
            if seen.insert(key.clone()) {
                self.occurrences.entry(key.clone()).or_default().push(index);
            }
            let (side, number, label) = outer_counter(&line.text());
            let anchor = match side {
                Some(CounterSide::Start) => line.rect.x0 / page.width,
                Some(CounterSide::End) => line.rect.x1 / page.width,
                None => line.rect.center().x / page.width,
            };
            self.observations
                .entry(index)
                .or_default()
                .push(Observation {
                    page: index,
                    key,
                    top: line.baseline < page.height * 0.18,
                    y: line.baseline / page.height,
                    anchor,
                    font: line.size,
                    side,
                    offset: number.map(|n| i64::from(n) - (index as i64 + 1)),
                    label: label_key(&label),
                    confirmed: false,
                });
        }
    }

    pub(crate) fn finish(&mut self) {
        let body = self
            .fonts
            .iter()
            .max_by_key(|(_, count)| **count)
            .map_or(12.0, |(size, _)| *size as f64 / 10.0);
        for observations in self.observations.values_mut() {
            // Use the document body font, not a diagram-heavy page's median.
            observations.retain(|candidate| candidate.font <= body * 1.15);
            for candidate in observations {
                candidate.confirmed = repeats_near_page(
                    &self.occurrences[&candidate.key],
                    candidate.page,
                    &candidate.key,
                );
            }
        }
    }

    pub(crate) fn removed_keys(&self, index: usize, active_labels: &[&str]) -> HashSet<String> {
        let mut removed = HashSet::new();
        let labels = active_labels
            .iter()
            .map(|l| label_key(l))
            .collect::<HashSet<_>>();
        for candidate in self.observations.get(&index).into_iter().flatten() {
            if candidate.confirmed {
                removed.insert(candidate.key.clone());
                continue;
            }
            // A page number alone is safe with the same counter/layout proof.
            // For a running title, an outline match is mandatory; an arbitrary
            // one-off line near the edge cannot inherit a header's classification.
            if candidate.offset.is_none()
                || (!candidate.label.is_empty() && !labels.contains(&candidate.label))
            {
                continue;
            }
            let support = self
                .observations
                .range(index.saturating_sub(10)..=index.saturating_add(10))
                .flat_map(|(_, observations)| observations)
                .filter(|other| {
                    other.confirmed
                        && other.page != index
                        && other.page % 2 == index % 2
                        && other.top == candidate.top
                        && other.side == candidate.side
                        && other.offset == candidate.offset
                        && (other.y - candidate.y).abs() <= 0.003
                        && (other.anchor - candidate.anchor).abs() <= 0.004
                        && (other.font - candidate.font).abs() <= candidate.font * 0.025
                })
                .map(|other| other.page)
                .collect::<HashSet<_>>();
            if support.len() >= 3 {
                removed.insert(candidate.key.clone());
            }
        }
        removed
    }
}
