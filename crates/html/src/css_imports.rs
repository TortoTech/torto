//! Bounded expansion of unconditional imports. Conditional at-rules remain
//! opaque, so their nested imports/rules never leak into ordinary screen styles.
use super::{PublicationUrl, StyleSheet, matching_brace, strip_css_comments};
use std::collections::{HashMap, HashSet};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILES: usize = 64;
const MAX_DEPTH: usize = 8;

#[derive(Default)]
pub(super) struct Budget {
    cached_bytes: usize,
    processed: usize,
    cache: HashMap<PublicationUrl, Option<String>>,
    active: HashSet<PublicationUrl>,
}
impl Budget {
    pub(super) fn load(
        &mut self,
        href: &PublicationUrl,
        load: &mut impl FnMut(&PublicationUrl) -> Option<String>,
    ) -> Option<String> {
        if let Some(css) = self.cache.get(href) {
            return css.clone();
        }
        if self.cache.len() >= MAX_FILES {
            return None;
        }
        let css = load(href).filter(|s| self.cached_bytes.saturating_add(s.len()) <= MAX_BYTES);
        self.cached_bytes += css.as_ref().map_or(0, String::len);
        self.cache.insert(href.clone(), css.clone());
        css
    }
}

pub(super) fn add(
    sheet: &mut StyleSheet,
    css: &str,
    base: &PublicationUrl,
    load: &mut impl FnMut(&PublicationUrl) -> Option<String>,
    budget: &mut Budget,
    depth: usize,
) {
    if depth > MAX_DEPTH
        || budget.processed.saturating_add(css.len()) > MAX_BYTES
        || !budget.active.insert(base.clone())
    {
        return;
    }
    budget.processed += css.len();
    let css = strip_css_comments(css);
    let mut rest = css.trim_start_matches('\u{feff}').trim_start();
    let mut allow_imports = true;
    while !rest.is_empty() {
        let Some((at, delimiter)) = delimiter(rest) else {
            break;
        };
        if delimiter == b';' {
            let statement = rest[..at].trim();
            if allow_imports
                && statement
                    .get(..7)
                    .is_some_and(|s| s.eq_ignore_ascii_case("@import"))
                && let Some(href) = import_href(&statement[7..])
                && let Ok(href) = base.resolve(href).map(|h| h.resource_url())
                && !budget.active.contains(&href)
                && let Some(css) = budget.load(&href, load)
            {
                add(sheet, &css, &href, load, budget, depth + 1);
            }
            // @charset and unsupported statement at-rules cannot swallow the
            // next selector, as they did when searching only for the next '{'.
            rest = rest[at + 1..].trim_start();
        } else {
            let Some(end) = matching_brace(rest, at) else {
                break;
            };
            let rule = &rest[..end + 1];
            sheet.add_css_with_base(rule, base);
            allow_imports = false;
            rest = rest[end + 1..].trim_start();
        }
    }
    budget.active.remove(base);
}

fn delimiter(value: &str) -> Option<(usize, u8)> {
    let (mut quote, mut escaped, mut parentheses) = (None, false, 0usize);
    for (i, b) in value.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            continue;
        }
        if matches!(b, b'\'' | b'"') {
            quote = Some(b);
            continue;
        }
        match b {
            b'(' => parentheses += 1,
            b')' => parentheses = parentheses.saturating_sub(1),
            b';' | b'{' if parentheses == 0 => return Some((i, b)),
            _ => {}
        }
    }
    None
}

fn import_href(value: &str) -> Option<&str> {
    let value = value.trim();
    let (url, tail) = if value
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("url("))
    {
        let end = value.find(')')?;
        (
            value[4..end].trim().trim_matches(['\'', '"']),
            value[end + 1..].trim(),
        )
    } else {
        let quote = value.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
        let end = value[1..].find(quote)? + 1;
        (&value[1..end], value[end + 1..].trim())
    };
    (!url.is_empty() && (tail.is_empty() || tail.eq_ignore_ascii_case("all"))).then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_resolve_relative_urls_reuse_reads_and_keep_conditional_rules_opaque() {
        let doc = roxmltree::Document::parse("<html><head><link rel='stylesheet' href='css/main.css'/></head><body><p>Hello</p></body></html>").unwrap();
        let base = PublicationUrl::parse("OPS/chapter.xhtml").unwrap();
        let mut reads = Vec::new();
        let sheet = StyleSheet::from_document(&doc, &base, &mut |href| {
            reads.push(href.path().to_owned());
            match href.path() {
                "OPS/css/main.css" => Some("@charset 'utf-8'; @import 'nested/base.css'; @import 'print.css' print; @media screen { @import 'hidden.css'; p { color:red; } } p { font-weight:bold; }".into()),
                "OPS/css/nested/base.css" => Some("@import '../main.css'; p { color:#00ff00; }".into()),
                _ => panic!("conditional stylesheet must not load: {href}"),
            }
        });
        assert_eq!(reads, ["OPS/css/main.css", "OPS/css/nested/base.css"]);
        let node = doc.descendants().find(|n| n.has_tag_name("p")).unwrap();
        let p = sheet.cascaded_properties(node);
        assert_eq!(p.get("color").map(String::as_str), Some("#00ff00"));
        assert_eq!(p.get("font-weight").map(String::as_str), Some("bold"));
    }
}
