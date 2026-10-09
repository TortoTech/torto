//! Cheap, deterministic preflight. Inspect native objects only; never render or OCR.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum TextRoute {
    Native,
    Ocr,
}

#[derive(Debug, Serialize)]
pub struct Assessment {
    pub route: TextRoute,
    pub pages: usize,
    pub sampled_pages: usize,
    pub native_pages: usize,
    pub scanned_pages: usize,
    pub unreliable_pages: usize,
    pub sparse_pages: usize,
}

/// Must run on a worker. Each sampled page is released before reading the next.
pub fn assess(path: &Path, book_id: &str, cancelled: &AtomicBool) -> Result<Assessment, String> {
    check_cancelled(cancelled)?;
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    check_cancelled(cancelled)?;
    let publication = open_with_id(bytes, "document.pdf", book_id).map_err(|e| e.to_string())?;
    let mut result = Assessment {
        route: TextRoute::Ocr,
        pages: publication.page_count,
        sampled_pages: 0,
        native_pages: 0,
        scanned_pages: 0,
        unreliable_pages: 0,
        sparse_pages: 0,
    };
    // Structural tags are unnecessary for this probe; avoid a whole-book tree walk.
    let tags = HashMap::new();
    for index in sample_pages(publication.page_count) {
        check_cancelled(cancelled)?;
        let page = extract::page(&publication.pdf, index, &tags);
        result.sampled_pages += 1;
        match page_kind(&page) {
            PageKind::Native => result.native_pages += 1,
            PageKind::Scanned => result.scanned_pages += 1,
            PageKind::Unreliable => result.unreliable_pages += 1,
            PageKind::Sparse => result.sparse_pages += 1,
        }
    }
    check_cancelled(cancelled)?;
    let informative = result.native_pages + result.scanned_pages + result.unreliable_pages;
    // Covers, blank pages and sparse illustrations do not outweigh body pages.
    // Uncertain/mixed documents use OCR; individual native conversion pages still
    // retain their existing fallback rules.
    if prefers_native(result.native_pages, informative) {
        result.route = TextRoute::Native;
    }
    Ok(result)
}

fn prefers_native(native: usize, informative: usize) -> bool {
    native > 0 && native * 4 >= informative * 3
}

fn sample_pages(count: usize) -> Vec<usize> {
    if count <= 12 {
        return (0..count).collect();
    }
    // Fixed stratified sampling across the body, plus two pages near the ends.
    // Do not base a long book's route on its cover or title page.
    let start = (count / 20).max(1);
    let end = count - start - 1;
    let mut pages = vec![1, count - 2];
    for i in 0..10 {
        pages.push(start + (end - start) * i / 9);
    }
    pages.sort_unstable();
    pages.dedup();
    pages
}

#[derive(Debug, PartialEq, Eq)]
enum PageKind {
    Native,
    Scanned,
    Unreliable,
    Sparse,
}

fn page_kind(page: &NativePage) -> PageKind {
    let count = page.glyphs.len();
    let chars = page
        .glyphs
        .iter()
        .flat_map(|g| g.text.chars())
        .filter(|c| !c.is_whitespace())
        .count();
    let covered = image_coverage(page) >= 0.8;
    // A searchable scan can have excellent Unicode mappings. Its hidden text
    // and large page image remain independent evidence of an OCR text layer.
    if (covered && (chars < 50 || page.invisible * 2 >= count.max(1)))
        || (count > 0 && page.invisible * 5 >= count * 4)
    {
        return PageKind::Scanned;
    }
    if chars < 50 {
        return PageKind::Sparse;
    }
    if bad_text(page) || layout::requires_page_fallback(page) {
        return PageKind::Unreliable;
    }
    PageKind::Native
}

pub(super) fn bad_text(page: &NativePage) -> bool {
    let mut chars = 0;
    let mut invalid = 0;
    let mut private = 0;
    let mut alphanumeric = 0;
    let mut whitespace = 0;
    let mut duplicates = 0;
    let mut positions = HashSet::new();
    for glyph in &page.glyphs {
        let position = (
            (glyph.rect[0] * 20.0).round() as i64,
            (glyph.rect[1] * 20.0).round() as i64,
            glyph.text.as_str(),
        );
        // Invisible word-spacing objects may deliberately share coordinates.
        // Only repeated printable characters are evidence of duplicate layers.
        if glyph.text.chars().any(|c| !c.is_whitespace()) {
            duplicates += usize::from(!positions.insert(position));
        }
        whitespace += glyph.text.chars().filter(|c| c.is_whitespace()).count();
        for c in glyph.text.chars().filter(|c| !c.is_whitespace()) {
            chars += 1;
            invalid += usize::from(matches!(c, '\u{fffd}' | '\u{fffc}') || c.is_control());
            private += usize::from(
                matches!(c as u32, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd),
            );
            alphanumeric += usize::from(c.is_alphanumeric());
        }
    }
    chars >= 50
        && (invalid > 6.max(chars * 3 / 100)
            || private > 6.max(chars * 5 / 100)
            || alphanumeric * 10 < chars * 3
            || whitespace * 3 > (chars + whitespace) * 2
            || duplicates > 6.max(page.glyphs.len() / 10)
            || page.unmapped * 20 > page.glyphs.len().max(1))
}

/// Union of clipped image rectangles, so tiled scans work and overlapping
/// illustrations are not counted twice. No bitmap decoding or page rendering.
fn image_coverage(page: &NativePage) -> f64 {
    let area = page.width * page.height;
    if !area.is_finite() || area <= 0.0 {
        return 0.0;
    }
    let rects: Vec<_> = page
        .images
        .iter()
        .filter_map(|r| {
            if !r.iter().all(|v| v.is_finite()) {
                return None;
            }
            let r = [
                r[0].max(0.0),
                r[1].max(0.0),
                r[2].min(page.width),
                r[3].min(page.height),
            ];
            (r[2] > r[0] && r[3] > r[1]).then_some(r)
        })
        .collect();
    // An unusual page with thousands of small images must not make preflight
    // quadratic. Use a conservative lower bound in this case.
    if rects.len() > 512 {
        return rects
            .iter()
            .map(|r| (r[2] - r[0]) * (r[3] - r[1]) / area)
            .fold(0.0, f64::max);
    }
    let mut xs: Vec<_> = rects.iter().flat_map(|r| [r[0], r[2]]).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    let mut covered = 0.0;
    for strip in xs.windows(2) {
        let mut ys: Vec<_> = rects
            .iter()
            .filter(|r| r[0] < strip[1] && r[2] > strip[0])
            .map(|r| (r[1], r[3]))
            .collect();
        ys.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut end = 0.0;
        let mut height = 0.0;
        for (a, b) in ys {
            height += (b - a.max(end)).max(0.0);
            end = end.max(b);
        }
        covered += (strip[1] - strip[0]) * height;
    }
    covered / area
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_page(text: &str) -> NativePage {
        NativePage {
            width: 600.0,
            height: 800.0,
            glyphs: text
                .chars()
                .enumerate()
                .map(|(index, c)| NativeGlyph {
                    text: c.to_string(),
                    rect: [
                        10.0 + (index % 50) as f64 * 10.0,
                        10.0 + (index / 50) as f64 * 15.0,
                        20.0 + (index % 50) as f64 * 10.0,
                        20.0 + (index / 50) as f64 * 15.0,
                    ],
                    baseline: [10.0, 20.0],
                    advance: [20.0, 20.0],
                    size: 12.0,
                    bold: false,
                    italic: false,
                    rotated: false,
                    tag: None,
                    mcid: None,
                    link: None,
                    index,
                    unmapped: false,
                })
                .collect(),
            ..Default::default()
        }
    }
    #[test]
    fn detects_searchable_scans_and_tiled_images() {
        let mut page = text_page(&"A readable sentence in the text layer. ".repeat(10));
        assert_eq!(page_kind(&page), PageKind::Native);
        page.images = vec![[0.0, 0.0, 300.0, 800.0], [300.0, 0.0, 600.0, 800.0]];
        page.invisible = page.glyphs.len();
        assert_eq!(image_coverage(&page), 1.0);
        assert_eq!(page_kind(&page), PageKind::Scanned);
        page.glyphs.clear();
        page.invisible = 0;
        assert_eq!(page_kind(&page), PageKind::Scanned);
    }
    #[test]
    fn rejects_corrupt_mapping_but_accepts_chinese_and_math_symbols() {
        let chinese = text_page(&"这是正确映射的中文文字，可以直接用于本地重排。".repeat(10));
        assert_eq!(page_kind(&chinese), PageKind::Native);
        let mut page = text_page(&"This is a normal sentence. ".repeat(10));
        page.glyphs[0].text = "∑αβ\u{e000}".into();
        assert_eq!(page_kind(&page), PageKind::Native);
        for glyph in page.glyphs.iter_mut().take(30) {
            glyph.text = "\u{e001}".into();
        }
        assert_eq!(page_kind(&page), PageKind::Unreliable);
        for glyph in page.glyphs.iter_mut().take(30) {
            glyph.text = "\u{fffd}".into();
        }
        assert_eq!(page_kind(&page), PageKind::Unreliable);
    }
    #[test]
    fn illustrations_and_blank_pages_are_not_evidence_of_bad_text() {
        let mut page = NativePage {
            width: 600.0,
            height: 800.0,
            ..Default::default()
        };
        assert_eq!(page_kind(&page), PageKind::Sparse);
        page.images = vec![[100.0, 100.0, 500.0, 500.0]; 4];
        assert!((image_coverage(&page) - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(page_kind(&page), PageKind::Sparse);
    }
    #[test]
    fn sampling_is_stable_bounded_and_spread_across_the_book() {
        for count in [0, 1, 12, 13, 50, 812, 10000] {
            let pages = sample_pages(count);
            assert_eq!(pages, sample_pages(count));
            assert!(pages.len() <= 12);
            assert!(pages.iter().all(|i| *i < count));
            assert!(pages.windows(2).all(|w| w[0] < w[1]));
            if count > 12 {
                assert!(pages.contains(&1));
                assert!(pages.contains(&(count - 2)));
            }
        }
    }

    #[test]
    fn mixed_documents_require_a_reliable_body_majority() {
        assert!(!prefers_native(0, 0));
        assert!(!prefers_native(2, 5));
        assert!(!prefers_native(7, 10));
        assert!(prefers_native(9, 12));
        // Sparse covers and blank pages are excluded from the informative count.
        assert!(prefers_native(2, 2));
    }

    #[test]
    fn duplicate_text_layers_and_excessive_spaces_are_unreliable() {
        let mut page = text_page(&"A normal readable sentence. ".repeat(10));
        let duplicates = page.glyphs.clone();
        page.glyphs.extend(duplicates);
        assert_eq!(page_kind(&page), PageKind::Unreliable);
        let page = text_page(&"A               word                ".repeat(25));
        assert_eq!(page_kind(&page), PageKind::Unreliable);
    }

    #[test]
    fn invisible_duplicate_word_spaces_do_not_reject_native_text() {
        let mut page = text_page(&"A normal readable sentence. ".repeat(10));
        let mut space = page.glyphs[0].clone();
        space.text = " ".into();
        page.glyphs.extend(std::iter::repeat_n(space, 60));
        page.invisible = 60;
        assert_eq!(page_kind(&page), PageKind::Native);
    }

    #[test]
    #[ignore = "local library diagnostic; TORTO_TEST_PDF_PATH"]
    fn local_quality_signals() {
        let path = std::env::var("TORTO_TEST_PDF_PATH").unwrap();
        let bytes = fs::read(&path).unwrap();
        let publication = open_with_id(bytes, "document.pdf", "diagnostic").unwrap();
        let mut native = 0;
        for index in sample_pages(publication.page_count) {
            let page = extract::page(&publication.pdf, index, &HashMap::new());
            let text: String = page.glyphs.iter().flat_map(|g| g.text.chars()).collect();
            let mut positions = HashSet::new();
            let duplicates = page
                .glyphs
                .iter()
                .filter(|g| {
                    !positions.insert((
                        (g.rect[0] * 20.0).round() as i64,
                        (g.rect[1] * 20.0).round() as i64,
                        g.text.as_str(),
                    ))
                })
                .count();
            let chars = text.chars().filter(|c| !c.is_whitespace()).count();
            let alpha = text.chars().filter(|c| c.is_alphanumeric()).count();
            let private = text
                .chars()
                .filter(|c| matches!(*c as u32, 0xe000..=0xf8ff))
                .count();
            let invalid = text
                .chars()
                .filter(|c| matches!(c, '\u{fffd}' | '\u{fffc}'))
                .count();
            let kind = page_kind(&page);
            native += usize::from(kind == PageKind::Native);
            eprintln!(
                "page={} kind={kind:?} glyphs={} chars={chars} alpha={alpha} private={private} invalid={invalid} unmapped={} invisible={} duplicate={duplicates} image={}",
                index + 1,
                page.glyphs.len(),
                page.unmapped,
                page.invisible,
                image_coverage(&page)
            );
        }
        assert!(
            native >= 8,
            "native body text must remain eligible for local reconstruction"
        );
    }
}
