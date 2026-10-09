//! Publication navigation relationships supplied to the HTML parser once per resource.
use super::*;

#[derive(Debug, Default)]
pub(super) struct HtmlContext {
    pub navigation_documents: Vec<PublicationUrl>,
    pub ancestors: HashMap<String, Vec<PublicationUrl>>,
    pub headings: HashMap<String, Vec<PublicationUrl>>,
}

impl HtmlContext {
    pub fn new(package: &PackageModel, xml: &str, base: &PublicationUrl, toc: &[TocEntry]) -> Self {
        let mut context = Self::default();
        context.navigation_documents.extend(
            package
                .manifest
                .values()
                .filter(|item| item.properties.iter().any(|p| p == "nav"))
                .map(|item| item.href.clone()),
        );
        if let Ok(document) = Document::parse(xml) {
            context.navigation_documents.extend(
                document
                    .descendants()
                    .filter(|node| {
                        node.has_tag_name("reference")
                            && attribute_local(*node, "type") == Some("toc")
                            && node.ancestors().any(|parent| parent.has_tag_name("guide"))
                    })
                    .filter_map(|node| {
                        attribute_local(node, "href").and_then(|href| base.resolve(href).ok())
                    }),
            );
        }
        fn visit(
            entries: &[TocEntry],
            ancestors: &mut Vec<PublicationUrl>,
            context: &mut HtmlContext,
        ) {
            for entry in entries {
                let href = entry.href.as_ref();
                if let Some(href) = href {
                    context
                        .headings
                        .entry(href.path().to_owned())
                        .or_default()
                        .push(href.clone());
                    let targets = context.ancestors.entry(href.path().to_owned()).or_default();
                    for ancestor in ancestors.iter() {
                        if !targets.contains(ancestor) {
                            targets.push(ancestor.clone());
                        }
                    }
                    ancestors.push(href.clone());
                }
                visit(&entry.children, ancestors, context);
                if href.is_some() {
                    ancestors.pop();
                }
            }
        }
        visit(toc, &mut Vec::new(), &mut context);
        context
    }
}
