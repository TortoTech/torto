use super::*;
use crate::font_data::Decoder;

pub(super) fn read(archive: &EpubArchive, package: &str) -> HashMap<String, Decoder> {
    let Some(map) = read_inner(archive, package) else {
        return HashMap::new();
    };
    map
}
fn read_inner(archive: &EpubArchive, package: &str) -> Option<HashMap<String, Decoder>> {
    let xml = archive
        .read_xml(&PublicationUrl::parse("META-INF/encryption.xml").ok()?)
        .ok()?;
    let encryption = Document::parse(&xml).ok()?;
    let opf = Document::parse(package).ok()?;
    let identifiers = opf
        .descendants()
        .filter(|n| n.has_tag_name(("http://purl.org/dc/elements/1.1/", "identifier")))
        .collect::<Vec<_>>();
    let unique = opf.root_element().attribute("unique-identifier");
    let primary = unique
        .and_then(|id| identifiers.iter().find(|n| n.attribute("id") == Some(id)))
        .or_else(|| identifiers.first())
        .map(|n| {
            n.descendants()
                .filter(Node::is_text)
                .filter_map(|n| n.text())
                .collect::<String>()
        })?;
    let adobe = Decoder::adobe(&primary).or_else(|| {
        identifiers
            .iter()
            .find_map(|n| normalized_node_text(*n).and_then(|s| Decoder::adobe(&s)))
    });
    let mut result = HashMap::new();
    for data in encryption
        .descendants()
        .filter(|n| n.has_tag_name("EncryptedData"))
        .take(128)
    {
        let algorithm = data
            .descendants()
            .find(|n| n.has_tag_name("EncryptionMethod"))
            .and_then(|n| n.attribute("Algorithm"));
        let uri = data
            .descendants()
            .find(|n| n.has_tag_name("CipherReference"))
            .and_then(|n| n.attribute("URI"));
        let decoder = match algorithm {
            Some("http://www.idpf.org/2008/embedding") => Decoder::idpf(&primary),
            Some("http://ns.adobe.com/pdf/enc#RC") => {
                let Some(d) = adobe.clone() else {
                    continue;
                };
                d
            }
            _ => continue,
        };
        if let Some(href) = uri.and_then(|uri| PublicationUrl::parse(uri).ok()) {
            result.insert(href.path().to_owned(), decoder);
        }
    }
    Some(result)
}
