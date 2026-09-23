use super::*;
use rebook_publication::{Book, BookSource, PublicationError, PublicationUrl, Resource};
use std::sync::{Arc, Mutex};

pub(super) struct Prepared {
    pub with_translation: bool,
    pub raw: Demand,
    pub demand: Demand,
    pub originals: HashMap<usize, Arc<Section>>,
    pub hashes: HashMap<usize, String>,
    pub inputs: HashMap<usize, Vec<(crate::plugins::TranslationBlockInput, SourceRange)>>,
}

struct CachedSource {
    source: Arc<dyn BookSource>,
    sections: Mutex<HashMap<usize, Arc<Section>>>,
}
impl BookSource for CachedSource {
    fn book(&self) -> &Book {
        self.source.book()
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        if let Some(section) = self.sections.lock().unwrap().get(&index) {
            return Ok((**section).clone());
        }
        let section = self.source.parse_section(index)?;
        self.sections
            .lock()
            .unwrap()
            .insert(index, Arc::new(section.clone()));
        Ok(section)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.source.resource(href)
    }
}

pub(super) fn prepare(
    source: Arc<dyn BookSource>,
    raw: Demand,
    originals: HashMap<usize, Arc<Section>>,
    fixed_page: bool,
    with_translation: bool,
) -> Prepared {
    let source = CachedSource {
        source,
        sections: Mutex::new(originals),
    };
    let mut demand = raw.clone();
    if with_translation {
        super::super::assistant::append_linked_footnote_translation_ranges(&source, &mut demand);
    }
    let mut hashes = HashMap::new();
    let mut inputs = HashMap::new();
    for (index, _) in &demand {
        if inputs.contains_key(index) {
            continue;
        }
        if let Ok(section) = source.parse_section(*index) {
            hashes.insert(*index, fingerprint(&section));
            inputs.insert(
                *index,
                crate::plugins::prepare_translation_inputs(&section, fixed_page),
            );
        }
    }
    Prepared {
        with_translation,
        raw,
        demand,
        originals: source.sections.into_inner().unwrap(),
        hashes,
        inputs,
    }
}
