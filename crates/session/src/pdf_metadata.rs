use serde::{Deserialize, Serialize};

/// Bibliographic metadata generated from the visual pages of a PDF.
///
/// Provider provenance remains part of the version-1 persistence contract, but
/// consumers should treat the title and authors as the semantic payload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedPdfMetadata {
    pub title: String,
    pub authors: Vec<String>,
    pub provider_name: String,
    pub model: String,
}

/// One generated PDF table-of-contents entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeneratedTocEntry {
    pub depth: usize,
    pub title: String,
    pub printed_page: String,
    /// One-based physical PDF page.
    pub physical_page: usize,
    pub confidence: f32,
}

/// Complete editable result of one generated PDF table-of-contents pass.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedTocDraft {
    pub provider_name: String,
    pub model: String,
    /// One-based physical PDF pages containing the printed contents.
    pub source_pages: Vec<usize>,
    pub entries: Vec<GeneratedTocEntry>,
}

/// Visually identified non-body page role used by the PDF OCR source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PdfOcrPageRole {
    Cover,
    TitlePage,
    BackCover,
}

/// Assignment of a visual PDF role to a one-based physical page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfOcrPageRoleAssignment {
    pub physical_page: usize,
    pub role: PdfOcrPageRole,
}

/// One toolkit-neutral mutation of persisted PDF-derived metadata.
///
/// Frontends submit this command after OCR/vision work or user review. The
/// shared coordinator normalizes it before the platform persistence adapter can
/// observe the values, so GPUI and the legacy desktop cannot diverge in how a
/// generated table of contents or page-role assignment is interpreted.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfDocumentMetadataCommand {
    ReplaceBibliographic(GeneratedPdfMetadata),
    ReplaceGeneratedToc(GeneratedTocDraft),
    ReplaceOcrPageRoles(Vec<PdfOcrPageRoleAssignment>),
}

impl PdfDocumentMetadataCommand {
    /// Normalizes externally generated data into the canonical session form.
    #[must_use]
    pub fn normalized(self) -> Self {
        match self {
            Self::ReplaceBibliographic(metadata) => {
                Self::ReplaceBibliographic(normalize_generated_pdf_metadata(metadata))
            }
            Self::ReplaceGeneratedToc(draft) => {
                Self::ReplaceGeneratedToc(normalize_generated_toc(draft))
            }
            Self::ReplaceOcrPageRoles(roles) => {
                Self::ReplaceOcrPageRoles(normalize_pdf_ocr_page_roles(roles))
            }
        }
    }
}

/// Persistence boundary for PDF-derived document metadata.
///
/// The target owns storage, sync dirtiness, and any platform transaction. It
/// receives only normalized, toolkit-neutral values.
pub trait PdfDocumentMetadataMutationTarget {
    type Error;

    fn replace_bibliographic(
        &mut self,
        book_id: &str,
        metadata: &GeneratedPdfMetadata,
    ) -> Result<(), Self::Error>;

    fn replace_generated_toc(
        &mut self,
        book_id: &str,
        draft: &GeneratedTocDraft,
    ) -> Result<(), Self::Error>;

    fn replace_ocr_page_roles(
        &mut self,
        book_id: &str,
        roles: &[PdfOcrPageRoleAssignment],
    ) -> Result<(), Self::Error>;
}

/// Normalizes and applies one PDF-derived metadata command.
///
/// The returned command is the exact normalized value observed by the target
/// and can be used to update an in-memory document session after persistence.
pub fn apply_pdf_document_metadata_command<T>(
    target: &mut T,
    book_id: &str,
    command: PdfDocumentMetadataCommand,
) -> Result<PdfDocumentMetadataCommand, T::Error>
where
    T: PdfDocumentMetadataMutationTarget + ?Sized,
{
    let command = command.normalized();
    match &command {
        PdfDocumentMetadataCommand::ReplaceBibliographic(metadata) => {
            target.replace_bibliographic(book_id, metadata)?;
        }
        PdfDocumentMetadataCommand::ReplaceGeneratedToc(draft) => {
            target.replace_generated_toc(book_id, draft)?;
        }
        PdfDocumentMetadataCommand::ReplaceOcrPageRoles(roles) => {
            target.replace_ocr_page_roles(book_id, roles)?;
        }
    }
    Ok(command)
}

/// Normalizes generated bibliographic metadata without consulting a frontend.
#[must_use]
pub fn normalize_generated_pdf_metadata(
    mut metadata: GeneratedPdfMetadata,
) -> GeneratedPdfMetadata {
    metadata.title = metadata.title.trim().to_owned();
    metadata.authors = metadata
        .authors
        .into_iter()
        .map(|author| author.trim().to_owned())
        .filter(|author| !author.is_empty())
        .collect();
    metadata.authors.dedup();
    metadata.provider_name = metadata.provider_name.trim().to_owned();
    metadata.model = metadata.model.trim().to_owned();
    metadata
}

/// Normalizes a generated table-of-contents draft into stable physical order.
#[must_use]
pub fn normalize_generated_toc(mut draft: GeneratedTocDraft) -> GeneratedTocDraft {
    draft.provider_name = draft.provider_name.trim().to_owned();
    draft.model = draft.model.trim().to_owned();
    draft.source_pages.retain(|page| *page > 0);
    draft.source_pages.sort_unstable();
    draft.source_pages.dedup();
    draft.entries = normalize_generated_toc_entries(draft.entries);
    draft
}

/// Normalizes generated entries while preserving the legacy page/depth policy.
#[must_use]
pub fn normalize_generated_toc_entries(
    mut entries: Vec<GeneratedTocEntry>,
) -> Vec<GeneratedTocEntry> {
    entries.retain(|entry| !entry.title.trim().is_empty() && entry.physical_page > 0);
    for entry in &mut entries {
        entry.title = entry.title.trim().to_owned();
        entry.printed_page = entry.printed_page.trim().to_owned();
        entry.confidence = entry.confidence.clamp(0.0, 1.0);
    }
    entries.sort_by(|left, right| {
        left.physical_page
            .cmp(&right.physical_page)
            .then_with(|| left.depth.cmp(&right.depth))
            .then_with(|| {
                toc_title_starts_with_number(&left.title)
                    .cmp(&toc_title_starts_with_number(&right.title))
            })
    });
    let mut previous_depth = 0;
    let mut previous_page = 0;
    let mut previous_title_was_numbered = false;
    for (index, entry) in entries.iter_mut().enumerate() {
        let title_is_numbered = toc_title_starts_with_number(&entry.title);
        entry.depth = if index == 0 {
            0
        } else if entry.depth == 0
            && previous_depth == 0
            && entry.physical_page == previous_page
            && !previous_title_was_numbered
            && title_is_numbered
        {
            1
        } else {
            entry.depth.min(previous_depth + 1)
        };
        previous_depth = entry.depth;
        previous_page = entry.physical_page;
        previous_title_was_numbered = title_is_numbered;
    }
    entries
}

/// Normalizes one-page-one-role assignments into stable physical order.
#[must_use]
pub fn normalize_pdf_ocr_page_roles(
    mut roles: Vec<PdfOcrPageRoleAssignment>,
) -> Vec<PdfOcrPageRoleAssignment> {
    roles.retain(|assignment| assignment.physical_page > 0);
    // Stable sorting makes the legacy "first assignment wins" behavior
    // deterministic when externally generated data repeats a page.
    roles.sort_by_key(|assignment| assignment.physical_page);
    roles.dedup_by_key(|assignment| assignment.physical_page);
    roles
}

fn toc_title_starts_with_number(title: &str) -> bool {
    title
        .trim_start()
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit() || ('０'..='９').contains(&character))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingTarget {
        commands: Vec<PdfDocumentMetadataCommand>,
    }

    impl PdfDocumentMetadataMutationTarget for RecordingTarget {
        type Error = std::convert::Infallible;

        fn replace_bibliographic(
            &mut self,
            _book_id: &str,
            metadata: &GeneratedPdfMetadata,
        ) -> Result<(), Self::Error> {
            self.commands
                .push(PdfDocumentMetadataCommand::ReplaceBibliographic(
                    metadata.clone(),
                ));
            Ok(())
        }

        fn replace_generated_toc(
            &mut self,
            _book_id: &str,
            draft: &GeneratedTocDraft,
        ) -> Result<(), Self::Error> {
            self.commands
                .push(PdfDocumentMetadataCommand::ReplaceGeneratedToc(
                    draft.clone(),
                ));
            Ok(())
        }

        fn replace_ocr_page_roles(
            &mut self,
            _book_id: &str,
            roles: &[PdfOcrPageRoleAssignment],
        ) -> Result<(), Self::Error> {
            self.commands
                .push(PdfDocumentMetadataCommand::ReplaceOcrPageRoles(
                    roles.to_vec(),
                ));
            Ok(())
        }
    }

    #[test]
    fn commands_normalize_before_the_target_observes_them() {
        let mut target = RecordingTarget::default();
        let applied = apply_pdf_document_metadata_command(
            &mut target,
            "book-a",
            PdfDocumentMetadataCommand::ReplaceOcrPageRoles(vec![
                PdfOcrPageRoleAssignment {
                    physical_page: 4,
                    role: PdfOcrPageRole::BackCover,
                },
                PdfOcrPageRoleAssignment {
                    physical_page: 0,
                    role: PdfOcrPageRole::Cover,
                },
                PdfOcrPageRoleAssignment {
                    physical_page: 4,
                    role: PdfOcrPageRole::TitlePage,
                },
                PdfOcrPageRoleAssignment {
                    physical_page: 1,
                    role: PdfOcrPageRole::Cover,
                },
            ]),
        )
        .unwrap();

        let expected = PdfDocumentMetadataCommand::ReplaceOcrPageRoles(vec![
            PdfOcrPageRoleAssignment {
                physical_page: 1,
                role: PdfOcrPageRole::Cover,
            },
            PdfOcrPageRoleAssignment {
                physical_page: 4,
                role: PdfOcrPageRole::BackCover,
            },
        ]);
        assert_eq!(applied, expected);
        assert_eq!(target.commands, [expected]);
    }

    #[test]
    fn generated_metadata_is_normalized() {
        let metadata = normalize_generated_pdf_metadata(GeneratedPdfMetadata {
            title: "  A Book  ".into(),
            authors: vec![" Alice ".into(), String::new(), "Alice".into()],
            provider_name: " Provider ".into(),
            model: " model ".into(),
        });
        assert_eq!(metadata.title, "A Book");
        assert_eq!(metadata.authors, ["Alice"]);
        assert_eq!(metadata.provider_name, "Provider");
        assert_eq!(metadata.model, "model");
    }

    #[test]
    fn generated_toc_is_stably_normalized() {
        let entry = |depth, title: &str, printed_page: &str, physical_page| GeneratedTocEntry {
            depth,
            title: title.into(),
            printed_page: printed_page.into(),
            physical_page,
            confidence: 1.2,
        };
        let draft = normalize_generated_toc(GeneratedTocDraft {
            provider_name: " provider ".into(),
            model: " model ".into(),
            source_pages: vec![4, 0, 2, 4],
            entries: vec![
                entry(0, "1 Child", " 2 ", 9),
                entry(0, " Parent ", "1", 9),
                entry(4, "Later", "3", 10),
            ],
        });

        assert_eq!(draft.provider_name, "provider");
        assert_eq!(draft.source_pages, [2, 4]);
        assert_eq!(draft.entries[0].title, "Parent");
        assert_eq!(draft.entries[1].depth, 1);
        assert_eq!(draft.entries[2].depth, 2);
        assert!((draft.entries[0].confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn public_payloads_keep_the_version_one_json_field_names() {
        let metadata = GeneratedPdfMetadata {
            title: "A Book".into(),
            authors: vec!["Alice".into()],
            provider_name: "provider".into(),
            model: "model".into(),
        };
        assert_eq!(
            serde_json::to_value(metadata).unwrap(),
            serde_json::json!({
                "title": "A Book",
                "authors": ["Alice"],
                "provider_name": "provider",
                "model": "model"
            })
        );
        assert_eq!(
            serde_json::to_value(PdfOcrPageRoleAssignment {
                physical_page: 2,
                role: PdfOcrPageRole::TitlePage,
            })
            .unwrap(),
            serde_json::json!({"physical_page": 2, "role": "title-page"})
        );
    }
}
