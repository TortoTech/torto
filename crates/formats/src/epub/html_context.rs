//! Publication navigation relationships supplied to the HTML parser once per resource.
use super::PackageModel;
use rebook_publication::{PublicationUrl, TocEntry};

pub(super) fn build(
    package: &PackageModel,
    xml: &str,
    base: &PublicationUrl,
    toc: &[TocEntry],
) -> crate::HtmlContext {
    let mut navigation_documents = Vec::new();
    navigation_documents.extend(
        package
            .manifest
            .values()
            .filter(|item| item.properties.iter().any(|p| p == "nav"))
            .map(|item| item.href.clone()),
    );
    navigation_documents.extend(crate::html_context::navigation_documents(xml, base));
    crate::HtmlContext::new(toc, navigation_documents)
}
