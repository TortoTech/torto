//! Conservative structural recovery for publishers that encode lists as paragraphs.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct ListItem {
    pub group: u64,
    pub ordered: bool,
    pub ordinal: u32,
    pub depth: u8,
    pub prefix_chars: u32,
}

#[derive(Clone, PartialEq)]
enum Family {
    Bullet(char),
    Number(char),
    Letter(char),
    Roman(char),
    Chinese(char),
    Circled,
    Image(String),
}

struct Candidate<'a, 'input> {
    node: Node<'a, 'input>,
    family: Family,
    ordinal: u32,
    prefix_chars: u32,
    inset: f32,
}

fn text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

fn excluded(node: Node<'_, '_>) -> bool {
    node.ancestors().filter(|n| n.is_element()).any(|n| {
        matches!(
            n.tag_name().name(),
            "nav" | "table" | "li" | "blockquote" | "figure" | "pre"
        ) || is_semantic_footnote_definition(n)
            || ["class", "role", "type"].iter().any(|attr| {
                n.attributes().filter(|a| a.name() == *attr).any(|a| {
                    a.value()
                        .to_ascii_lowercase()
                        .split_whitespace()
                        .any(|word| {
                            word.contains("footnote")
                                || word.contains("endnote")
                                || word.contains("bibliograph")
                                || word.contains("reference")
                                || word == "bibitem"
                                || word.contains("heading")
                                || word.contains("title")
                                || word == "notes"
                                || word == "doc-toc"
                        })
                })
            })
    })
}

fn reference_heading(node: Node<'_, '_>) -> bool {
    if !matches!(
        node.tag_name().name(),
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
    ) {
        return false;
    }
    // A linked paragraph label can be a contents entry or breadcrumb. It must
    // not turn the following chapter body into a reference region.
    if node.tag_name().name() == "p"
        && node
            .descendants()
            .any(|child| child.attribute("href").is_some())
    {
        return false;
    }
    let label = text(node).trim().to_lowercase();
    matches!(
        label.as_str(),
        "notes"
            | "endnotes"
            | "footnotes"
            | "references"
            | "bibliography"
            | "further reading"
            | "recommended reading"
            | "注释"
            | "注釋"
            | "参考文献"
            | "參考文獻"
    )
}

fn text_marker(value: &str) -> Option<(Family, u32, u32)> {
    let value = value.trim_start();
    let first = value.chars().next()?;
    if "•●◦▪▫‣⁃·◇◆♦◊○■□※»".contains(first) {
        let body = value[first.len_utf8()..].trim_start();
        return body
            .chars()
            .any(char::is_alphanumeric)
            .then_some((Family::Bullet(first), 1, 1));
    }
    if ('①'..='⑳').contains(&first) {
        if !value[first.len_utf8()..].chars().any(char::is_alphanumeric) {
            return None;
        }
        return Some((Family::Circled, first as u32 - '①' as u32 + 1, 1));
    }
    // Hyphens and dashes also introduce signatures, dialogue and quotations.
    // Recover them only from explicit HTML lists, never from this weak signal.
    let (value, opening) = match first {
        '(' | '（' | '[' => (&value[first.len_utf8()..], Some(first)),
        _ => (value, None),
    };
    let (token, delimiter) = value
        .char_indices()
        .find(|(_, c)| matches!(c, '.' | ')' | '、' | '．' | '）' | ']'))?;
    if opening
        .is_some_and(|open| !matches!((open, delimiter), ('(', ')') | ('（', '）') | ('[', ']')))
    {
        return None;
    }
    let label = &value[..token];
    let body = value[token + delimiter.len_utf8()..].trim_start();
    if body.is_empty() || body.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None; // decimal section numbers, equations and dates
    }
    let (family, ordinal) = if label.len() <= 3 && label.bytes().all(|c| c.is_ascii_digit()) {
        let number: u32 = label.parse().ok()?;
        if number == 0 || number > 100 {
            return None;
        }
        (Family::Number(opening.unwrap_or(delimiter)), number)
    } else if let Some(ordinal) = ["i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x"]
        .iter()
        .position(|roman| *roman == label.to_ascii_lowercase())
    {
        (
            Family::Roman(opening.unwrap_or(delimiter)),
            ordinal as u32 + 1,
        )
    } else if let Some(ordinal) = ["一", "二", "三", "四", "五", "六", "七", "八", "九", "十"]
        .iter()
        .position(|number| *number == label)
    {
        (
            Family::Chinese(opening.unwrap_or(delimiter)),
            ordinal as u32 + 1,
        )
    } else if label.len() == 1 && label.bytes().all(|c| c.is_ascii_alphabetic()) {
        (
            Family::Letter(delimiter),
            u32::from(label.as_bytes()[0].to_ascii_lowercase() - b'a') + 1,
        )
    } else {
        return None;
    };
    Some((
        family,
        ordinal,
        label.chars().count() as u32 + 1 + u32::from(opening.is_some()),
    ))
}

fn small_image_marker(styles: &StyleSheet, node: Node<'_, '_>, size: [u32; 2]) -> bool {
    let [w, h] = size;
    if w == 0 || h == 0 {
        return false;
    }
    let ratio = w as f32 / h as f32;
    let style = styles.image_style(node);
    let pixels = |v| {
        if let Some(rebook_publication::ImageLength::Pixels(p)) = v {
            Some(p)
        } else {
            None
        }
    };
    let (w, h) = match (pixels(style.width), pixels(style.height)) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w / ratio),
        (None, Some(h)) => (h * ratio, h),
        _ => (w as f32, h as f32),
    };
    w.is_finite()
        && h.is_finite()
        && w > 0.0
        && h > 0.0
        && w <= 32.0
        && h <= 32.0
        && w <= h * 3.0
        && h <= w * 3.0
}

fn candidate<'a, 'input>(
    node: Node<'a, 'input>,
    styles: &StyleSheet,
    href: &PublicationUrl,
    image_dimensions: &mut impl FnMut(&PublicationUrl) -> Option<[u32; 2]>,
    dimensions: &mut HashMap<PublicationUrl, Option<[u32; 2]>>,
) -> Option<Candidate<'a, 'input>> {
    if node.tag_name().name() != "p"
        || excluded(node)
        || has_explicit_paragraph_list_marker(node)
        || contains_display_math(node)
        || is_numbered_media_paragraph(node)
    {
        return None;
    }
    let value = text(node);
    let leading = node.descendants().skip(1).find(|n| {
        n.is_text() && n.text().is_some_and(|t| !t.trim().is_empty())
            || n.is_element() && matches!(n.tag_name().name(), "img" | "math")
    })?;
    // Leading linked numbers in note sections are backlink labels, not list markers.
    if leading.ancestors().take_while(|n| *n != node).any(|n| {
        n.is_element()
            && n.tag_name().name() == "a"
            && n.attribute("href").is_some_and(|href| href.contains('#'))
    }) {
        return None;
    }
    // Most paragraphs have no candidate prefix. Avoid CSS work for them.
    let textual_marker = if leading.is_element() {
        None
    } else {
        Some(text_marker(&value)?)
    };
    let style = styles.block_style(node, BlockStyle::default());
    let (family, ordinal, prefix_chars) = if leading.is_element() {
        let class = node.attribute("class").unwrap_or("").to_ascii_lowercase();
        if leading.tag_name().name() != "img"
            || !value.chars().any(char::is_alphabetic)
            || !(style.indent < -0.5 || class.contains("list") || class.contains("bullet"))
            || leading.attribute("alt").is_some_and(|alt| {
                let alt = alt.to_ascii_lowercase();
                alt.contains("equation") || alt.contains("formula")
            })
        {
            return None;
        }
        let image_href = href.resolve(leading.attribute("src")?).ok()?;
        let size = *dimensions
            .entry(image_href.clone())
            .or_insert_with(|| image_dimensions(&image_href));
        if !small_image_marker(styles, leading, size?) {
            return None;
        }
        (Family::Image(image_href.to_string()), 1, 0)
    } else {
        textual_marker?
    };
    Some(Candidate {
        node,
        family,
        ordinal,
        prefix_chars,
        inset: style
            .margin_start_fraction
            .mul_add(1_000.0, style.margin_start),
    })
}

pub(super) fn detect(
    root: Node<'_, '_>,
    styles: &StyleSheet,
    href: &PublicationUrl,
    image_dimensions: &mut impl FnMut(&PublicationUrl) -> Option<[u32; 2]>,
) -> HashMap<roxmltree::NodeId, ListItem> {
    let mut result = HashMap::new();
    let mut dimensions = HashMap::new();
    // Native lists also encode bullets/ordinals as images. Repeated small
    // leading images are marker evidence; an isolated content glyph is not.
    let native_lists = root.descendants().filter(|n| n.is_element() && matches!(n.tag_name().name(), "ul" | "ol")).map(|list| {
        let items = list.children().filter(|n| n.has_tag_name("li")).collect::<Vec<_>>();
        let leading = items.iter().map(|item| {
                    item.descendants().skip(1).find(|n| {
                        n.is_text() && n.text().is_some_and(|s| !s.trim().is_empty())
                || n.is_element() && matches!(n.tag_name().name(), "img" | "math")
                    })
                })
            .collect::<Vec<_>>();
        (list, items, leading)
    }).collect::<Vec<_>>();
    // A one-item list can use the same bullet as the surrounding lists.
    let mut repeats = HashMap::<&str, usize>::new();
    for image in native_lists.iter().flat_map(|(_, _, leading)| leading.iter().flatten()).filter(|n| n.has_tag_name("img")) {
        if let Some(src) = image.attribute("src") { *repeats.entry(src).or_default() += 1; }
    }
    for (list, items, leading) in native_lists {
        for (ordinal, (item, first)) in items.iter().zip(&leading).enumerate() {
            let Some(image) = first.filter(|n| n.has_tag_name("img")) else { continue;
            };
            let Some(src) = image.attribute("src") else { continue;
            };
            let numbered = image.attribute("alt").is_some_and(|s| {
                !s.trim().is_empty() && s.trim().chars().all(|c| c.is_ascii_digit() || ".()、".contains(c))
            });
            if (!numbered && repeats.get(src).copied().unwrap_or(0) < 2)
                || styles.visually_hidden(image) || !text(*item).chars().any(char::is_alphabetic) { continue;
            }
            let Ok(url) = href.resolve(src) else { continue };
            let size = *dimensions.entry(url.clone()).or_insert_with(|| image_dimensions(&url));
            if !size.is_some_and(|size| small_image_marker(styles, image, size)) { continue;
            }
            result.insert(item.id(), ListItem { group: list.range().start as u64,
                ordered: list.has_tag_name("ol"), ordinal: ordinal as u32 + 1, depth: 0, prefix_chars: 0,
                },
            );
        }
    }
    let mut reference_nodes = HashSet::new();
    let mut reference_section = false;
    for node in root.descendants().filter(|n| n.is_element()) {
        if reference_heading(node) {
            reference_section = true;
        } else if matches!(
            node.tag_name().name(),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
        ) {
            reference_section = reference_heading(node);
        }
        if reference_section && node.tag_name().name() == "p" {
            reference_nodes.insert(node.id());
        }
    }
    // Each parent is a separate scope. Lists cannot jump across headings, tables,
    // quotations, ordinary prose or another publisher container.
    for parent in root
        .descendants()
        .filter(|n| n.is_element() && !excluded(*n))
    {
        let mut run: Vec<Candidate<'_, '_>> = Vec::new();
        let mut references = false;
        let flush = |run: &mut Vec<Candidate<'_, '_>>, result: &mut HashMap<_, _>| {
            if run.len() >= 2 {
                let group = run[0].node.range().start as u64;
                let mut indents = vec![run[0].inset];
                for item in run.iter() {
                    if let Some(depth) = indents.iter().position(|i| (item.inset - i).abs() <= 4.0)
                    {
                        indents.truncate(depth + 1);
                    } else if item.inset > *indents.last().unwrap() + 4.0 {
                        indents.push(item.inset);
                    } else {
                        indents.clear();
                        indents.push(item.inset);
                    }
                    result.insert(
                        item.node.id(),
                        ListItem {
                            group,
                            ordered: !matches!(item.family, Family::Bullet(_) | Family::Image(_)),
                            ordinal: item.ordinal,
                            prefix_chars: item.prefix_chars,
                            depth: (indents.len() - 1).min(u8::MAX as usize) as u8,
                        },
                    );
                }
            }
            run.clear();
        };
        for node in parent.children().filter(|n| n.is_element()) {
            if matches!(
                node.tag_name().name(),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            ) {
                references = reference_heading(node);
            }
            // Empty anchor paragraphs carry source targets, but no list boundary.
            if node.tag_name().name() == "p"
                && text(node).trim().is_empty()
                && !has_descendant_image(node)
            {
                continue;
            }
            let next = (!references && !reference_nodes.contains(&node.id()))
                .then(|| candidate(node, styles, href, image_dimensions, &mut dimensions))
                .flatten();
            if let Some(mut next) = next {
                // I/V/X can be alphabetic or Roman markers. Resolve the
                // ambiguity from the surrounding sequence rather than spelling.
                let letter_ordinal = |roman| match roman {
                    1 => Some(9),
                    5 => Some(22),
                    10 => Some(24),
                    _ => None,
                };
                let single = run.len() == 1;
                if let Some(last) = run.last_mut() {
                    match (&last.family, &next.family) {
                        (Family::Letter(a), Family::Roman(b)) if a == b => {
                            if let Some(ordinal) = letter_ordinal(next.ordinal)
                                .filter(|ordinal| *ordinal == last.ordinal + 1)
                            {
                                next.family = Family::Letter(*a);
                                next.ordinal = ordinal;
                            }
                        }
                        (Family::Roman(a), Family::Letter(b)) if single && a == b => {
                            if let Some(ordinal) = letter_ordinal(last.ordinal)
                                .filter(|ordinal| *ordinal + 1 == next.ordinal)
                            {
                                last.family = Family::Letter(*b);
                                last.ordinal = ordinal;
                            }
                        }
                        _ => {}
                    }
                }
                if run.last().is_some_and(|last| {
                    last.family != next.family
                        || (!matches!(next.family, Family::Bullet(_) | Family::Image(_))
                            && next.ordinal != last.ordinal + 1)
                }) {
                    flush(&mut run, &mut result);
                }
                run.push(next);
            } else {
                flush(&mut run, &mut result);
            }
        }
        flush(&mut run, &mut result);
    }
    result
}

pub(super) fn prefix_length(content: &[Inline], marker_chars: u32) -> u32 {
    let value: String = content
        .iter()
        .take_while(|inline| !matches!(inline, Inline::Break | Inline::Math(_)))
        .filter_map(|inline| {
            if let Inline::Text(run) = inline {
                Some(run.text.as_str())
            } else {
                None
            }
        })
        .collect();
    let mut chars = value.chars().peekable();
    let mut count = 0;
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
        count += 1;
    }
    for _ in 0..marker_chars {
        if chars.next().is_some() {
            count += 1;
        }
    }
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str, size: Option<[u32; 2]>) -> Section {
        let descriptor = SpineItem {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("OPS/chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        };
        parse_section_with_image_metadata(
            &format!("<html><body>{body}</body></html>"),
            &descriptor,
            |_| None,
            |_| false,
            |_| size,
            SectionParseHints::default(),
        )
        .unwrap()
    }

    fn items(section: &Section) -> Vec<&TextBlock> {
        section
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(t) if matches!(t.kind, TextBlockKind::ListItem { .. }) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn high_resolution_bullets_use_css_display_size_but_content_images_do_not() {
        for width in ["16px", "1em"] {
            let body = format!(
                "<p>Consider these examples:</p><p style='margin-left:24px;text-indent:-16px'><img style='width:{width}' src='bullet.jpg'/> First item.</p><p style='margin-left:24px;text-indent:-16px'><img style='width:{width}' src='bullet.jpg'/> Second item.</p>"
            );
            let section = parse(&body, Some([156, 141]));
            assert_eq!(items(&section).len(), 2, "{width}");
            assert_eq!(
                items(&section)[0].style.list_group,
                items(&section)[1].style.list_group
            );
            let large = parse(&body.replace(width, "128px"), Some([156, 141]));
            assert!(items(&large).is_empty());
        }
    }

    #[test]
    fn image_markers_preserve_text_and_anchor_locations() {
        let body = r#"<p id="intro">Use two corpora:</p><p><a id="empty"/></p><div>
            <p class="bulllist" id="first"><a id="target"/><img src="diamond.gif" alt="image"/> A large general corpus.</p>
            <p class="bulllist"><img src="diamond.gif"/> A document of specific context.</p></div><p>Afterwards.</p>"#;
        let section = parse(body, Some([9, 10]));
        let original = parse(body, None);
        let found = items(&section);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].style.list_group, found[1].style.list_group);
        assert_eq!(found[0].style.authored_list_marker, Some(1));
        for (actual, expected) in section.blocks.iter().zip(&original.blocks) {
            if let (Block::Text(a), Block::Text(b)) = (actual, expected) {
                assert_eq!(a.content, b.content);
                assert_eq!(a.source, b.source);
            }
        }
        assert_eq!(section.anchors, original.anchors);
    }

    #[test]
    fn bare_markers_and_chinese_numbering_form_separate_groups() {
        for body in [
            "<p>● First item.</p><p>● Second item.</p>",
            "<p>·旋转刺激。</p><p>·横向刺激。</p>",
            "<p>1.和莉莉建立信任关系。</p><p>2.莉莉读过这本书。</p>",
            "<p>A.是</p><p>B.不是</p>",
            "<p>①第一项</p><p>②第二项</p>",
            "<p>(1) First.</p><p>(2) Second.</p>",
            "<p>（1）第一项</p><p>（2）第二项</p>",
            "<p>i. First.</p><p>ii. Second.</p>",
            "<p>一、第一项</p><p>二、第二项</p>",
        ] {
            let section = parse(&format!("{body}<p>Boundary.</p>{body}"), None);
            let found = items(&section);
            assert_eq!(found.len(), 4, "{body}");
            assert_ne!(found[0].style.list_group, found[2].style.list_group);
            assert!(
                found
                    .iter()
                    .all(|item| item.style.authored_list_marker.is_some())
            );
        }
    }

    #[test]
    fn weak_or_non_list_signals_stay_prose() {
        for body in [
            "<p>● One isolated item.</p>",
            "<p>1.2 First heading.</p><p>1.3 Second heading.</p>",
            "<p>1949. First year.</p><p>1950. Second year.</p>",
            "<p>1. First item.</p><p>3. Unrelated item.</p>",
            "<p>- Ben Shneiderman</p><p>- Don Norman</p>",
            "<h2>References</h2><p>1. First reference.</p><p>2. Second reference.</p>",
            "<h2>References</h2><div><p>1. First reference.</p><p>2. Second reference.</p></div>",
            "<p class=\"bibliography\">REFERENCES</p><p class=\"bibitem\">(4) Author A. (2005). Title.</p><p class=\"bibitem\">(5) Author B. (2006). Title.</p>",
            "<p>References</p><div><p>1. Author A.</p><p>2. Author B.</p></div>",
            "<p class=\"note\"><a href=\"chapter.xhtml#n1\">1.</a> First note.</p><p><a href=\"chapter.xhtml#n2\">2.</a> Second note.</p>",
            "<p class=\"bulllist\"><img src=\"formula.gif\"/></p><p class=\"bulllist\"><img src=\"formula.gif\"/></p>",
            "<p class=\"bulllist\"><img src=\"photo.gif\"/> Photo caption.</p><p class=\"bulllist\"><img src=\"photo.gif\"/> Another photo.</p>",
        ] {
            assert!(items(&parse(body, Some([128, 128]))).is_empty(), "{body}");
        }
    }

    #[test]
    fn inferred_bullets_recover_hanging_indent_depth() {
        let section = parse(
            r#"<p style="margin-left:24px;text-indent:-12px">• Root.</p>
            <p style="margin-left:48px;text-indent:-12px">• Nested.</p>
            <p style="margin-left:24px;text-indent:-12px">• Root again.</p>"#,
            None,
        );
        let depths: Vec<_> = items(&section)
            .iter()
            .map(|t| match t.kind {
                TextBlockKind::ListItem { depth, .. } => depth,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(depths, vec![0, 1, 0]);
    }

    #[test]
    fn roman_and_alphabetic_sequences_use_their_neighboring_structure() {
        for (labels, ordinals) in [
            (vec!["H", "I", "J"], vec![8, 9, 10]),
            (vec!["I", "J"], vec![9, 10]),
            (vec!["IV", "V", "VI"], vec![4, 5, 6]),
            (vec!["I", "II", "III"], vec![1, 2, 3]),
        ] {
            let body = labels
                .iter()
                .map(|label| format!("<p>{label}. Item body.</p>"))
                .collect::<String>();
            let section = parse(&body, None);
            assert_eq!(
                items(&section)
                    .iter()
                    .map(|item| match item.kind {
                        TextBlockKind::ListItem { ordinal, .. } => ordinal,
                        _ => unreachable!(),
                    })
                    .collect::<Vec<_>>(),
                ordinals
            );
        }
    }

    #[test]
    fn linked_contents_labels_do_not_suppress_body_lists() {
        let section = parse(
            "<p><a href=\"notes.xhtml\">Notes</a></p><p>● First item.</p><p>● Second item.</p>",
            None,
        );
        assert_eq!(items(&section).len(), 2);
    }

    #[test]
    fn late_note_region_detection_discards_inferred_lists() {
        let document =
            Document::parse("<body><p>1. First note.</p><p>2. Second note.</p></body>").unwrap();
        let root = document.root_element();
        let href = PublicationUrl::parse("OPS/chapter.xhtml").unwrap();
        let styles = StyleSheet::from_document(&document, &href, &mut |_| None);
        let inferred = detect(root, &styles, &href, &mut |_| None);
        assert_eq!(inferred.len(), 2);
        let mut separator = |_: &PublicationUrl| false;
        let mut parser = ReadingIrParser::new(
            SpineItemId::new("chapter").unwrap(),
            href,
            styles,
            HashMap::new(),
            &mut separator,
        );
        parser.inferred_lists = inferred;
        parser.parse_note_section_children(root).unwrap();
        assert!(parser.inferred_lists.is_empty());
        for block in parser.blocks {
            let Block::Note(note) = block else {
                panic!("expected grouped note");
            };
            for block in note.blocks {
                if let Block::Text(text) = block {
                    assert_eq!(text.style.authored_list_marker, None);
                    assert!(!matches!(text.kind, TextBlockKind::ListItem { .. }));
                }
            }
        }
    }
}
