# Local egui_extras patch

Based on the crates.io 0.36.2 release, licensed under MIT OR Apache-2.0.

Backports the table portion of merged upstream [egui #8316](https://github.com/emilk/egui/pull/8316), paired with the egui Context/pass-state patch. Virtual row spacers expand the UI extent without consuming automatic widget IDs. Header, body and cells use explicit stable IDs. The upstream `scope_id` API is represented by 0.36.2's `UiBuilder::id` and parent `Ui::id`. The spacer regression test is retained.

Remove both backports together after a stable upstream release includes #8316 and table/diagnostic regressions pass.
