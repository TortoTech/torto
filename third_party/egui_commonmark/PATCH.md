# Local egui_commonmark compatibility patch

This directory vendors `egui_commonmark 0.25.0` from crates.io. Upstream 0.25 now supports egui
0.36 directly; the path copy remains paired with Torto's patched `egui_commonmark_backend`, which
keeps layout sentinels out of text selection and supplies the reader's strong-text and citation
presentation.

Remove this path copy together with the backend patch after the remaining selection and presentation
customizations can use upstream extension points.
