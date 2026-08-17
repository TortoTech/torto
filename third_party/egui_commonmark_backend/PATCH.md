# Local egui_commonmark backend patch

This directory vendors `egui_commonmark_backend 0.25.0` from crates.io.

Torto keeps pure layout newlines non-selectable so cross-widget Markdown selections do not cover
list markers. It also provides the reader-owned strong font family and compact citation-link icon.
The upstream 0.25 fix for empty image links is retained.

Remove this patch after upstream selection handling or public rendering hooks can express these
behaviors without replacing the backend.
