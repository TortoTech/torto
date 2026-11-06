//! Resolve ambiguous navigation against authored headings on section workers.
//! Book URLs/IDs remain unchanged; virtual anchors exist only in prepared IR.
use super::*;

#[derive(Clone)]
pub(super) struct Target {
    pub href: PublicationUrl,
    pub source: Option<SourceAnchor>,
    pub active: bool,
}

#[derive(Default)]
pub(super) struct Resolution {
    pub targets: HashMap<String, Target>,
    pub layout: HashSet<String>,
    pub reading: Vec<Option<String>>,
}

fn key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn heading_text(text: &TextBlock) -> String {
    text.content
        .iter()
        .map(|i| match i {
            Inline::Break => " ".into(),
            Inline::Math(m) => m.original_text().unwrap_or_else(|| m.latex.clone()),
            _ => i
                .text_runs()
                .iter()
                .map(|r| r.text.as_str())
                .collect::<String>(),
        })
        .collect()
}

pub(super) fn prepare(
    source: &dyn BookSource,
    index: usize,
    rows: &[TocViewItem],
    section: &mut Section,
) -> Result<Resolution, PublicationError> {
    let mut result = Resolution::default();
    if source.book().sections[index]
        .properties
        .iter()
        .any(|p| p == rebook_publication::CONTINUATION_SECTION_PROPERTY)
    {
        result.reading.push(None);
    }
    let authored: HashMap<_, _> = section
        .anchors
        .iter()
        .map(|a| (a.fragment.clone(), a.source.clone()))
        .collect();
    // Siblings with different labels pointing to the exact same file start are
    // ambiguous. Parent/child aliases and valid authored fragments stay intact.
    let repairable = source.book().metadata.layout != RenditionLayout::PrePaginated;
    let mut groups: HashMap<(Option<&str>, &PublicationUrl), (&str, String, bool)> = HashMap::new();
    if repairable {
        for row in rows
            .iter()
            .filter(|r| r.target.as_ref().is_some_and(|t| t.fragment().is_none()))
        {
            let group = groups
                .entry((
                    row.ancestors.last().map(String::as_str),
                    row.target.as_ref().unwrap(),
                ))
                .or_insert_with(|| (row.id.as_str(), key(&row.label), false));
            group.2 |= group.1 != key(&row.label);
        }
    }
    let ambiguous = |row: &TocViewItem| {
        groups
            .get(&(
                row.ancestors.last().map(String::as_str),
                row.target.as_ref().unwrap(),
            ))
            .is_some_and(|g| g.2)
    };
    let needs_repair = |row: &TocViewItem| {
        ambiguous(row)
            || row
                .target
                .as_ref()
                .and_then(PublicationUrl::fragment)
                .is_some_and(|f| !authored.contains_key(f))
    };
    let repair = repairable && rows.iter().any(needs_repair);
    let original = if repair {
        source.navigation_section(index)?
    } else {
        None
    };
    let original = original.as_ref().unwrap_or(section);
    let mut headings: HashMap<String, Vec<SourceAnchor>> = HashMap::new();
    if repair {
        for block in &original.blocks {
            // Restrict recovery to real headings; a quote, code sample or TOC
            // label mentioning the same words must never become the target.
            if let Block::Text(text) = block
                && matches!(text.kind, TextBlockKind::Heading(_))
                && let Some(range) = &text.source
            {
                let label = key(&heading_text(text));
                if !label.is_empty() {
                    headings.entry(label).or_default().push(range.start.clone());
                }
            }
        }
    }
    if repair {
        for pair in original.blocks.windows(2) {
            if let [Block::Text(ordinal), Block::Text(title)] = pair
                && matches!(ordinal.kind, TextBlockKind::HeadingOrdinal(_))
                && matches!(title.kind, TextBlockKind::Heading(_))
                && let Some(source) = &ordinal.source
            {
                let combined = key(&format!(
                    "{} {}",
                    heading_text(ordinal),
                    heading_text(title)
                ));
                headings
                    .entry(combined)
                    .or_default()
                    .push(source.start.clone());
            }
        }
    }
    let mut visible_sources: HashMap<String, Vec<SourceRange>> = HashMap::new();
    if repair {
        for block in &section.blocks {
            let mut ranges = Vec::new();
            append_block_geometry_sources(block, &mut ranges);
            for range in ranges {
                visible_sources
                    .entry(range.start.node.clone())
                    .or_default()
                    .push(range);
            }
        }
    }
    let semantic_original_anchors = original.anchors.clone();
    let first_source = section
        .blocks
        .iter()
        .find_map(block_source)
        .map(|r| r.start.clone());
    let mut virtual_anchors = Vec::new();
    for row in rows {
        let href = row.target.as_ref().unwrap();
        let mut effective = href.clone();
        let existing = href.fragment().and_then(|f| authored.get(f)).cloned();
        let mut recovered = None;
        if repairable && needs_repair(row) {
            if let Some(candidates) = headings.get(&key(&row.label))
                && let [heading] = candidates.as_slice()
            {
                recovered = Some(heading.clone());
            }
            // A unique explicit copyright identifier is a last resort when a
            // converted front-matter page has no title. Never search prose.
            if recovered.is_none()
                && matches!(key(&row.label).as_str(), "copyright" | "版权" | "版权页")
            {
                let candidates = semantic_original_anchors
                    .iter()
                    .filter(|a| {
                        matches!(
                            key(&a.fragment).as_str(),
                            "copyright" | "copyright-page" | "copyrightpage"
                        )
                    })
                    .collect::<Vec<_>>();
                if let [anchor] = candidates.as_slice() {
                    recovered = Some(anchor.source.clone());
                }
            }
            if recovered.is_none()
                && matches!(key(&row.label).as_str(), "copyright" | "版权" | "版权页")
            {
                let notices = original
                    .blocks
                    .iter()
                    .take(64)
                    .filter_map(|block| {
                        let Block::Text(text) = block else {
                            return None;
                        };
                        let notice = heading_text(text);
                        (text.kind == TextBlockKind::Paragraph
                            && notice.trim_start().starts_with('©')
                            && notice
                                .as_bytes()
                                .windows(4)
                                .any(|year| year.iter().all(u8::is_ascii_digit)))
                        .then(|| text.source.as_ref().map(|r| r.start.clone()))
                        .flatten()
                    })
                    .collect::<Vec<_>>();
                if let [notice] = notices.as_slice() {
                    recovered = Some(notice.clone());
                }
            }
            // Source membership is checked after view overlays: a hidden or
            // replaced heading cannot publish a dangling navigation anchor.
            if recovered.as_ref().is_some_and(|a| {
                !visible_sources
                    .get(&a.node)
                    .is_some_and(|ranges| ranges.iter().any(|r| source_range_contains(r, a)))
            }) {
                recovered = None;
            }
        }
        let active = !ambiguous(row)
            || recovered.is_some()
            || groups
                .get(&(row.ancestors.last().map(String::as_str), href))
                .is_some_and(|group| group.0 == row.id);
        let target_source = recovered
            .clone()
            .or(existing)
            .or_else(|| first_source.clone());
        if let Some(anchor) = recovered {
            let mut fragment = format!("torto-toc-{}", row.id.replace('/', "-"));
            while authored.contains_key(&fragment)
                || virtual_anchors
                    .iter()
                    .any(|a: &SectionAnchor| a.fragment == fragment)
            {
                fragment.push('_');
            }
            effective = href.resource_url().resolve(&format!("#{fragment}"))?;
            virtual_anchors.push(SectionAnchor {
                page_label: None,
                fragment,
                source: anchor,
            });
        }
        result.targets.insert(
            row.id.clone(),
            Target {
                href: effective,
                source: target_source,
                active,
            },
        );
    }
    if repair {
        let mut order = HashMap::new();
        for block in &original.blocks {
            let mut ranges = Vec::new();
            append_block_geometry_sources(block, &mut ranges);
            for range in ranges {
                let next = order.len();
                order.entry(range.start.node).or_insert(next);
            }
        }
        let ranks = rows
            .iter()
            .map(|row| {
                let target = &result.targets[&row.id];
                let original = row.target.as_ref().unwrap();
                let known = target.active
                    && (target.href != *original
                        || original
                            .fragment()
                            .is_some_and(|f| authored.contains_key(f))
                        || original.fragment().is_none());
                known
                    .then(|| {
                        target
                            .source
                            .as_ref()
                            .and_then(|a| order.get(&a.node).map(|i| (*i, a.text_offset)))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        let mut before = vec![None; rows.len() + 1];
        let mut after: Vec<Option<(usize, u64)>> = vec![None; rows.len() + 1];
        for i in 0..rows.len() {
            before[i + 1] = before[i].max(ranks[i]);
        }
        for i in (0..rows.len()).rev() {
            after[i] = match (after[i + 1], ranks[i]) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        for (i, row) in rows.iter().enumerate() {
            let target = result.targets.get_mut(&row.id).unwrap();
            let original = row.target.as_ref().unwrap();
            if target.href != *original
                && ranks[i].is_some_and(|rank| {
                    before[i].is_some_and(|left| rank < left)
                        || after[i + 1].is_some_and(|right| rank > right)
                })
            {
                // Reject both sides of an inverted recovery pair. Valid
                // authored URLs stay intact and define hard ordering bounds.
                target.href = original.clone();
                target.source = first_source.clone();
                target.active = false;
            }
        }
        let retained = result
            .targets
            .values()
            .filter_map(|t| t.href.fragment())
            .collect::<HashSet<_>>();
        virtual_anchors.retain(|a| retained.contains(a.fragment.as_str()));
    }
    for row in rows {
        let target = &result.targets[&row.id];
        if !target.active {
            continue;
        }
        if let Some(fragment) = target.href.fragment()
            && row.depth == 0
        {
            result.layout.insert(fragment.to_owned());
        }
        let boundary = target.href.fragment().map(str::to_owned);
        if !result.reading.contains(&boundary) {
            result.reading.push(boundary);
        }
    }
    section.anchors.extend(virtual_anchors);
    Ok(result)
}

impl ReaderSession {
    fn effective_toc_target(&self, item: &TocViewItem) -> Option<Target> {
        let href = item.target.as_ref()?;
        let index = self.section_index_for_href(href)?;
        self.repository
            .get(index)
            .and_then(|s| s.toc_targets.get(&item.id).cloned())
            .or_else(|| {
                Some(Target {
                    href: href.clone(),
                    source: None,
                    active: true,
                })
            })
    }
    /// Memory-only source anchor for one TOC row, including recovered targets.
    pub fn source_anchor_for_toc(&self, id: &str) -> Option<SourceAnchor> {
        let item = self.toc_items.iter().find(|r| r.id == id)?;
        self.effective_toc_target(item)?.source
    }
    /// Navigate by stable row identity; the authored URL may be ambiguous.
    pub fn go_to_toc(&mut self, id: &str) -> Result<NavigationResult, ReaderError> {
        let row = self
            .toc_items
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| ReaderError::NavigationTargetNotFound(id.into()))?;
        let index = row
            .target
            .as_ref()
            .and_then(|t| self.section_index_for_href(t))
            .ok_or_else(|| ReaderError::NavigationTargetNotFound(id.into()))?;
        let _section = self.repository.load(index)?;
        let target = self.effective_toc_target(&row).unwrap();
        let result = self.go_to_href(&target.href)?;
        Ok(self.toc_navigation_result(result, &row, target.active))
    }
    fn toc_navigation_result(
        &self,
        mut result: NavigationResult,
        row: &TocViewItem,
        active: bool,
    ) -> NavigationResult {
        if active {
            result.snapshot.active_toc_id = Some(row.id.clone());
            result.snapshot.active_toc_path = row.ancestors.clone();
            if row.has_children {
                result.snapshot.active_toc_path.push(row.id.clone());
            }
        } else {
            result.snapshot = self.snapshot();
        }
        result
    }
    /// Never parse or paginate on the caller/UI thread.
    pub fn try_go_to_toc(&mut self, id: &str) -> Result<NavigationAttempt, ReaderError> {
        self.poll_prefetch()?;
        let row = self
            .toc_items
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| ReaderError::NavigationTargetNotFound(id.into()))?;
        let index = row
            .target
            .as_ref()
            .and_then(|t| self.section_index_for_href(t))
            .ok_or_else(|| ReaderError::NavigationTargetNotFound(id.into()))?;
        if self.repository.get(index).is_none()
            && !self.try_ensure_navigation_segment(SegmentKey {
                section_index: index,
                segment_index: 0,
            })?
        {
            return Ok(NavigationAttempt::Pending);
        }
        let target = self.effective_toc_target(&row).unwrap();
        match self.try_go_to_href(&target.href)? {
            NavigationAttempt::Pending => Ok(NavigationAttempt::Pending),
            NavigationAttempt::Ready(result) => Ok(NavigationAttempt::Ready(
                self.toc_navigation_result(result, &row, target.active),
            )),
        }
    }

    pub(super) fn toc_position(&self, item: &TocViewItem) -> Option<ReaderPosition> {
        let href = item.target.as_ref()?;
        let section = self.repository.get(self.section_index_for_href(href)?);
        match section.as_ref().and_then(|s| s.toc_targets.get(&item.id)) {
            Some(target) => target
                .active
                .then(|| self.position_for_href(&target.href))
                .flatten(),
            None => self.position_for_href(href),
        }
    }
    /// Exact authored hyperlink hit; destinations remain publication URLs.
    pub fn book_link_at_page(
        &mut self,
        position: ReaderPosition,
        x: f32,
        y: f32,
    ) -> Result<Option<(PublicationUrl, rebook_publication::LinkRole, String)>, ReaderError> {
        self.ensure_segment(SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        })?;
        let page = self.page_at(position)?;
        let Some(hit) = page.hit_test_text(x, y, true) else {
            return Ok(None);
        };
        let Some(range) =
            page.text_region_source_range(hit.region_index, hit.cluster_start..hit.cluster_end)
        else {
            return Ok(None);
        };
        let Some(section) = self.repository.get(position.section_index) else {
            return Ok(None);
        };
        let Some(fragment) = section.fragment_for_source(&range.start) else {
            return Ok(None);
        };
        Ok(rebook_publication::links::at_source(
            &section.fragments[fragment].blocks,
            &range.start,
        ))
    }
    pub fn book_link_at_current_spread(
        &mut self,
        x: f32,
        y: f32,
    ) -> Result<Option<(PublicationUrl, rebook_publication::LinkRole, String)>, ReaderError> {
        for (position, _, offset) in self.current_spread_pages()?.into_iter().rev() {
            if let Some(link) = self.book_link_at_page(position, x - offset, y)? {
                return Ok(Some(link));
            }
        }
        Ok(None)
    }
    // Exact source intervals take precedence over a page number. No layout of
    // sibling targets is needed, even after translation or viewport changes.
    pub(super) fn active_toc_at_source(&self, anchor: &SourceAnchor) -> Option<&TocViewItem> {
        let section = self.repository.get(self.current_section)?;
        self.toc_at_source_in_section(self.current_section, &section, anchor)
    }

    fn toc_at_source_in_section(&self, index: usize, section: &PreparedSection, anchor: &SourceAnchor) -> Option<&TocViewItem> {
        let point = section
            .source_order
            .get(&anchor.node)
            .map(|i| (*i, anchor.text_offset))?;
        self.toc_index.items_by_section[index]
            .iter()
            .filter_map(|&i| {
                let row = &self.toc_items[i];
                let target = section.toc_targets.get(&row.id)?;
                if !target.active {
                    return None;
                }
                let source = target.source.as_ref()?;
                let order = (*section.source_order.get(&source.node)?, source.text_offset);
                (order <= point).then_some(((order, i), row))
            })
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, row)| row)
    }

    /// Resolve a batch of note/highlight anchors in source order. Each involved
    /// section is prepared once, reusing the same repaired TOC boundaries as
    /// navigation; no page layout, position change or whole-book cache is needed.
    pub fn source_contexts(&self, anchors: &[SourceAnchor]) -> Vec<Option<ReaderSourceContext>> {
        let mut groups: std::collections::BTreeMap<usize, Vec<(usize, &SourceAnchor)>> = Default::default();
        let mut result = vec![None; anchors.len()];
        for (slot, anchor) in anchors.iter().enumerate() {
            if let Some(index) = self.source.book().sections.iter().position(|s| s.id == anchor.spine) {
                groups.entry(index).or_default().push((slot, anchor));
            }
        }
        for (index, group) in groups {
            let prepared = self.repository.load(index).ok();
            for (slot, anchor) in group {
                let toc = prepared.as_ref().and_then(|section| self.toc_at_source_in_section(index, section, anchor))
                    .or_else(|| {
                        // A chunk without its own TOC continues the preceding unit.
                        self.toc_index.items_by_section[index].is_empty().then(|| {
                            self.toc_index.preceding_section_by_section[index]
                                .and_then(|previous| self.toc_index.items_by_section[previous].last())
                                .map(|&row| &self.toc_items[row])
                        }).flatten()
                    });
                result[slot] = Some(ReaderSourceContext {
                    section_index: index,
                    node_order: prepared.as_ref().and_then(|s| s.source_order.get(&anchor.node).copied()),
                    toc: toc.cloned(),
                });
            }
        }
        result
    }
}

#[cfg(test)]
mod local_library_tests;
#[cfg(test)]
mod tests;
