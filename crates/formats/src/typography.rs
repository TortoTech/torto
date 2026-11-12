//! Bounded, source-wide typography analysis. Authored styles and source ranges stay intact.
use rebook_publication::{
    Block, Book, BookSource, Inline, PublicationError, PublicationUrl, Resource, Section,
    TextAlignment, TextBlock, TextBlockKind,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

fn text(t: &TextBlock) -> String {
    t.content
        .iter()
        .flat_map(Inline::text_runs)
        .map(|r| r.text.as_str())
        .collect()
}
fn body(t: &TextBlock) -> bool {
    t.kind == TextBlockKind::Paragraph && t.source.is_some() && t.style.paragraph_group.is_none()
}
fn counts(t: &TextBlock) -> (usize, usize) {
    t.content
        .iter()
        .flat_map(Inline::text_runs)
        .fold((0, 0), |(n, b), r| {
            let c = r.text.chars().filter(|c| !c.is_whitespace()).count();
            (n + c, b + if r.style.bold { c } else { 0 })
        })
}
fn numbered(value: &str) -> bool {
    let prefix = value.split_whitespace().next().unwrap_or("");
    let numbered = prefix.trim_end_matches(['.', ')', '．', '）', '、', ':', '：']);
    (!numbered.is_empty()
        && numbered
            .chars()
            .all(|c| c.is_numeric() || matches!(c, '.' | '．'))
        && prefix.len() < value.len())
        || value
            .strip_prefix('第')
            .is_some_and(|v| v.chars().take(12).any(|c| "章节篇部卷回".contains(c)))
}
fn recover_headings(section: &mut Section, bold_baseline: bool) {
    let mut sizes: Vec<f32> = section
        .blocks
        .iter()
        .filter_map(|b| {
            let Block::Text(t) = b else { return None };
            if !body(t) || counts(t).0 < 100 {
                return None;
            }
            let runs: Vec<_> = t.content.iter().flat_map(Inline::text_runs).collect();
            let n: usize = runs.iter().map(|r| r.text.chars().count()).sum();
            Some(
                runs.iter()
                    .map(|r| r.style.size_scale * r.text.chars().count() as f32)
                    .sum::<f32>()
                    / n.max(1) as f32,
            )
        })
        .take(512)
        .filter(|s| s.is_finite() && *s > 0.)
        .collect();
    sizes.sort_by(f32::total_cmp);
    let base = sizes.get(sizes.len() / 2).copied().unwrap_or(1.);
    let count = section.blocks.len();
    for i in 0..count {
        // Authored figure/table/quote/list neighbourhoods are stronger evidence
        // than visual emphasis. Stop looking when ordinary prose separates them.
        let mut protected = false;
        for forward in [false, true] {
            for distance in 1..=3 {
                let at = if forward {
                    i.checked_add(distance)
                } else {
                    i.checked_sub(distance)
                };
                let Some(block) = at.and_then(|at| section.blocks.get(at)) else {
                    break;
                };
                match block {
                    Block::Separator(_) => {}
                    Block::Text(t)
                        if t.kind.is_heading()
                            || counts(t).0 >= 60 && t.kind == TextBlockKind::Paragraph =>
                    {
                        break;
                    }
                    Block::Text(t) if t.kind == TextBlockKind::Paragraph => {}
                    _ => {
                        protected = true;
                        break;
                    }
                }
            }
        }
        if protected {
            continue;
        }
        let next_prose = section
            .blocks
            .get(i + 1)
            .is_some_and(|b| matches!(b, Block::Text(t) if body(t) && counts(t).0 >= 60));
        let Block::Text(t) = &mut section.blocks[i] else {
            continue;
        };
        if !body(t) {
            continue;
        }
        let value = text(t);
        let value = value.trim();
        let (n, bold) = counts(t);
        let cjk = value.chars().any(|c| matches!(c as u32, 0x3400..=0x9fff));
        if n == 0
            || bold * 10 < n * 9
            || n > if cjk { 48 } else { 120 }
            || value.split_whitespace().count() > 20
            || value.ends_with(['.', '。', '．'])
            || !value.chars().any(char::is_alphabetic)
        {
            continue;
        }
        if t.content.iter().any(|v| match v {
            Inline::Text(r) => {
                r.style.inline_role != rebook_publication::InlineRole::Normal
                    || r.style.link_role != rebook_publication::LinkRole::Normal
                    || r.style.inline_citation != 0
            }
            Inline::Break => false,
            _ => true,
        }) {
            continue;
        }
        if value.contains(['=', '∑', '∫', '≤', '≥', '→']) || value.starts_with('<') {
            continue;
        }
        let lower = value.to_lowercase();
        if [
            "http://",
            "https://",
            "www.",
            ".com",
            "isbn",
            "copyright",
            "版权所有",
        ]
        .iter()
        .any(|p| lower.contains(p))
        {
            continue;
        }
        if value.strip_prefix(['图', '表']).is_some_and(|v| {
            v.trim_start()
                .chars()
                .next()
                .is_some_and(|c| c.is_numeric() || "一二三四五六七八九十".contains(c))
        }) {
            continue;
        }
        if [
            "source:",
            "source：",
            "figure ",
            "fig.",
            "table ",
            "—",
            "–",
            "©",
            "来源",
            "图注",
            "表注",
        ]
        .iter()
        .any(|p| lower.starts_with(p))
        {
            continue;
        }
        let size = t
            .content
            .iter()
            .flat_map(Inline::text_runs)
            .map(|r| {
                r.style.size_scale * r.text.chars().filter(|c| !c.is_whitespace()).count() as f32
            })
            .sum::<f32>()
            / n as f32;
        if size < base * 0.9
            || value.starts_with(['“', '「', '『']) && value.ends_with(['”', '」', '』'])
        {
            continue;
        }
        let centered = t.style.authored_alignment.unwrap_or(t.style.align) == TextAlignment::Center;
        // When bold is the book's normal body face it is not independent evidence.
        if bold_baseline && size < base * 1.12 {
            continue;
        }
        if centered && (size >= base * 1.1 || next_prose) || numbered(value) && next_prose {
            t.kind = TextBlockKind::Heading(3);
        }
    }
}

pub(super) fn wrap(inner: Arc<dyn BookSource>) -> Arc<dyn BookSource> {
    if inner.book().metadata.layout != rebook_publication::RenditionLayout::Reflowable
        || inner.body_bold_profile().is_some()
    {
        return inner;
    }
    static PROFILES: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let cache = PROFILES.get_or_init(Default::default);
    let key = format!(
        "{}:{:?}:{}",
        inner.book().id,
        inner.content_revision(),
        inner.book().sections.len()
    );
    let cached = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .copied();
    let normalize = cached.unwrap_or_else(|| {
        // Prefer authored TOC sections over image-only spine slices. A picture
        // essay can contain dozens of SVG pages between its prose chapters.
        fn toc_sections(
            entries: &[rebook_publication::TocEntry],
            book: &Book,
            out: &mut Vec<usize>,
        ) {
            for entry in entries {
                let label = entry.label.trim().to_lowercase();
                let front = [
                    "cover",
                    "title page",
                    "copyright page",
                    "copyright",
                    "contents",
                    "table of contents",
                    "封面",
                    "版权",
                    "目录",
                    "扉页",
                ]
                .contains(&label.as_str());
                if !front
                    && let Some(href) = &entry.href
                    && let Some(index) = book
                        .sections
                        .iter()
                        .position(|s| s.linear && s.href.resource_url() == href.resource_url())
                    && !out.contains(&index)
                {
                    out.push(index);
                }
                toc_sections(&entry.children, book, out);
            }
        }
        let mut linear = Vec::new();
        toc_sections(&inner.book().table_of_contents, inner.book(), &mut linear);
        if linear.len() < 2 {
            linear = inner
                .book()
                .sections
                .iter()
                .enumerate()
                .filter_map(|(i, s)| (s.linear && s.media_type != "image/svg+xml").then_some(i))
                .collect();
        }
        linear.sort_unstable();
        let mut chars = 0usize;
        let mut bold = 0usize;
        let mut paragraphs = 0usize;
        let mut all_bold = 0usize;
        let mut regions = 0;
        // At most five evenly spaced sections, 24 body paragraphs per section and 128 KiB total.
        for slot in 0..linear.len().min(5) {
            let at = slot * linear.len() / linear.len().min(5);
            let Ok(mut section) = inner.parse_section(linear[at]) else {
                continue;
            };
            recover_headings(&mut section, false);
            let mut found = false;
            let mut taken = 0;
            for block in &section.blocks {
                let Block::Text(t) = block else { continue };
                if !body(t) {
                    continue;
                }
                let (n, b) = counts(t);
                if n < 100 {
                    continue;
                }
                taken += 1;
                let sampled = n.min(4096).min(128 * 1024 - chars);
                chars += sampled;
                bold += b * sampled / n;
                paragraphs += 1;
                all_bold += usize::from(b * 10 >= n * 9);
                found = true;
                if taken >= 24 || chars >= 128 * 1024 {
                    break;
                }
            }
            regions += usize::from(found);
            if chars >= 128 * 1024 {
                break;
            }
        }
        let result = regions >= 2
            && paragraphs >= 20
            && chars >= 4000
            && bold * 100 >= chars * 85
            && all_bold * 100 >= paragraphs * 65;
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 64 {
            if let Some(key) = cache.keys().next().cloned() {
                cache.remove(&key);
            }
        }
        cache.insert(key, result);
        result
    });
    Arc::new(TypographySource { inner, normalize })
}
struct TypographySource {
    inner: Arc<dyn BookSource>,
    normalize: bool,
}
impl TypographySource {
    fn prepare(&self, mut section: Section) -> Section {
        fn frontmatter(entries: &[rebook_publication::TocEntry], href: &PublicationUrl) -> bool {
            entries.iter().any(|entry| {
                let label = entry.label.trim().to_lowercase();
                entry
                    .href
                    .as_ref()
                    .is_some_and(|url| url.resource_url() == href.resource_url())
                    && [
                        "cover",
                        "title page",
                        "copyright",
                        "copyright page",
                        "contents",
                        "table of contents",
                        "封面",
                        "扉页",
                        "版权",
                        "版权信息",
                        "目录",
                    ]
                    .contains(&label.as_str())
                    || frontmatter(&entry.children, href)
            })
        }
        if !frontmatter(&self.inner.book().table_of_contents, &section.href) {
            recover_headings(&mut section, self.normalize);
        }
        for block in &mut section.blocks {
            let Block::Text(t) = block else { continue };
            if !self.normalize || !body(t) || counts(t).0 < 100 {
                continue;
            }
            for inline in &mut t.content {
                match inline {
                    Inline::Text(r) => r.style.body_bold = r.style.bold && !r.style.bold_emphasis,
                    Inline::Ruby(r) => {
                        for run in r.base.iter_mut().chain(&mut r.annotation) {
                            run.style.body_bold = run.style.bold && !run.style.bold_emphasis;
                        }
                    }
                    _ => {}
                }
            }
        }
        section
    }
}
impl BookSource for TypographySource {
    fn inner_source(&self) -> Option<&dyn BookSource> {
        Some(self.inner.as_ref())
    }
    fn body_bold_profile(&self) -> Option<bool> {
        Some(self.normalize)
    }
    fn book(&self) -> &Book {
        self.inner.book()
    }
    fn content_revision(&self) -> Option<u64> {
        self.inner.content_revision()
    }
    fn parse_section(&self, i: usize) -> Result<Section, PublicationError> {
        self.inner.parse_section(i).map(|s| self.prepare(s))
    }
    fn parse_section_without_body_margins(&self, i: usize) -> Result<Section, PublicationError> {
        self.inner
            .parse_section_without_body_margins(i)
            .map(|s| self.prepare(s))
    }
    fn navigation_section(&self, i: usize) -> Result<Option<Section>, PublicationError> {
        self.parse_section(i).map(Some)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.inner.resource(href)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{DirectBookSource, SectionContent, SourceBook, SourceSection};
    fn source(id: &str, html: String) -> Arc<dyn BookSource> {
        Arc::new(
            DirectBookSource::open(
                SourceBook {
                    id: id.into(),
                    metadata: Default::default(),
                    sections: (0..2)
                        .map(|_| SourceSection {
                            title: "Chapter".into(),
                            content: SectionContent::Html(html.clone()),
                            linear: true,
                            properties: vec![],
                        })
                        .collect(),
                    table_of_contents: vec![],
                    resources: vec![],
                    cover_path: None,
                },
                crate::BookFormat::Epub,
            )
            .unwrap(),
        )
    }
    fn prose() -> String {
        "A substantial ordinary paragraph establishes the typography of the book and carries no title or other special semantic role. ".repeat(3)
    }
    fn texts(section: &Section) -> Vec<&TextBlock> {
        section
            .blocks
            .iter()
            .filter_map(|b| {
                if let Block::Text(t) = b {
                    Some(t)
                } else {
                    None
                }
            })
            .collect()
    }
    #[test]
    fn global_css_and_complete_strong_wrappers_normalize_only_body_typography() {
        for global in [false, true] {
            let para = format!("{}<strong>local emphasis</strong>{}", prose(), prose());
            let body = if global {
                format!(
                    "<style>p{{font-weight:bold}}</style>{}<p><b>Daily</b></p>",
                    format!("<p>{para}</p>").repeat(12)
                )
            } else {
                format!(
                    "{}<p><b>Daily</b></p>",
                    format!("<p><strong>{para}</strong></p>").repeat(12)
                )
            };
            let raw = source(
                if global {
                    "global-bold"
                } else {
                    "wrapper-bold"
                },
                body,
            );
            let before = raw.parse_section(0).unwrap();
            let source = wrap(raw);
            assert_eq!(source.body_bold_profile(), Some(true));
            let after = source.parse_section(0).unwrap();
            let t = texts(&after);
            assert_eq!(before.anchors, after.anchors);
            assert_eq!(text(t[0]), text(texts(&before)[0]));
            let runs: Vec<_> = t[0].content.iter().flat_map(Inline::text_runs).collect();
            assert!(runs.iter().any(|r| r.style.body_bold));
            assert!(
                runs.iter()
                    .filter(|r| r.text == "local emphasis")
                    .all(|r| r.style.bold_emphasis && !r.style.body_bold)
            );
            let label = t.last().unwrap();
            assert_eq!(text(label), "Daily");
            assert!(
                label
                    .content
                    .iter()
                    .flat_map(Inline::text_runs)
                    .all(|r| r.style.bold && !r.style.body_bold)
            );
        }
    }
    #[test]
    fn isolated_whole_paragraph_emphasis_is_preserved() {
        let html = format!(
            "{}<p><strong>{}</strong></p>",
            format!("<p>{}</p>", prose()).repeat(20),
            prose()
        );
        let source = wrap(source("isolated-bold", html));
        assert_eq!(source.body_bold_profile(), Some(false));
        assert!(
            texts(&source.parse_section(0).unwrap())
                .last()
                .unwrap()
                .content
                .iter()
                .flat_map(Inline::text_runs)
                .all(|r| r.style.bold && !r.style.body_bold)
        );
    }
    #[test]
    fn style_headings_recover_before_ai_without_promoting_sentences_credits_or_captions() {
        let html = format!(
            "<style>.h{{text-align:center;font-size:large}}</style><p class='h'><b>An Elephantine History</b></p><p>{}</p><p class='h'><b>A complete emphasized sentence.</b></p><p class='h'><b>—John Smith</b></p><p class='h'><b>Figure 1. Sample</b></p>",
            prose()
        );
        let raw = source("style-headings", html);
        let before = raw.parse_section(0).unwrap();
        let source = wrap(raw);
        let after = source.parse_section(0).unwrap();
        let t = texts(&after);
        assert_eq!(t[0].kind, TextBlockKind::Heading(3));
        assert_eq!(t[0].source, texts(&before)[0].source);
        assert!(t[1..].iter().all(|t| t.kind == TextBlockKind::Paragraph));
    }
    #[test]
    fn css_reset_and_global_strong_rule_do_not_remove_local_emphasis() {
        let html = format!(
            "<style>strong{{font-weight:bold}}p{{font-weight:bold}}.plain{{font-weight:normal}}</style>{}<p class='plain'>Normal <strong>important</strong> words.</p>",
            format!("<p>{}</p>", prose()).repeat(12)
        );
        let source = wrap(source("reset-bold", html));
        assert_eq!(source.body_bold_profile(), Some(true));
        let section = source.parse_section(0).unwrap();
        let last = texts(&section).pop().unwrap();
        let runs: Vec<_> = last.content.iter().flat_map(Inline::text_runs).collect();
        assert!(
            runs.iter()
                .any(|r| r.text == "important" && r.style.bold && r.style.bold_emphasis)
        );
        assert!(
            runs.iter()
                .filter(|r| r.text != "important")
                .all(|r| !r.style.bold)
        );
    }
    #[test]
    fn neighbouring_media_urls_and_formula_text_are_not_style_headings() {
        let html = format!(
            "<p style='text-align:center'><b>A diagram caption</b></p><img src='missing.png'/><p>{}</p><p style='text-align:center'><b>Visit www.example.com for this book</b></p><p>{}</p><p style='text-align:center'><b>E = mc2</b></p><p>{}</p><p style='text-align:center'><b><a href='https://example.org'>Discover your next read</a></b></p>",
            prose(),
            prose(),
            prose()
        );
        let source = wrap(source("heading-exclusions", html));
        assert!(
            texts(&source.parse_section(0).unwrap())
                .iter()
                .all(|t| !t.kind.is_heading())
        );
    }
    #[test]
    fn sampling_reads_at_most_five_sections_and_only_the_first_24_body_paragraphs() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counted {
            inner: Arc<dyn BookSource>,
            book: Book,
            calls: Arc<AtomicUsize>,
        }
        impl BookSource for Counted {
            fn book(&self) -> &Book {
                &self.book
            }
            fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                self.inner.parse_section(index % 2)
            }
            fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
                self.inner.resource(href)
            }
        }
        let html = format!(
            "{}{}",
            format!("<p>{}</p>", prose()).repeat(24),
            format!("<p><strong>{}</strong></p>", prose()).repeat(200)
        );
        let inner = source("bounded-typography-sample", html);
        let mut book = inner.book().clone();
        book.sections = (0..100)
            .map(|i| {
                let mut section = book.sections[0].clone();
                section.href = PublicationUrl::parse(&format!("section-{i}.xhtml")).unwrap();
                section
            })
            .collect();
        book.table_of_contents.clear();
        let calls = Arc::new(AtomicUsize::new(0));
        let wrapped = wrap(Arc::new(Counted {
            inner,
            book,
            calls: calls.clone(),
        }));
        assert_eq!(calls.load(Ordering::Relaxed), 5);
        assert_eq!(
            wrapped.body_bold_profile(),
            Some(false),
            "Later paragraphs must not leak beyond the sample budget"
        );
    }
}
