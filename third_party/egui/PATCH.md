# Local egui patch

This directory vendors `egui 0.36.2` from crates.io.

Torto's AI Chat allows a text selection to autoscroll beyond the visible viewport. Upstream
`Label::ui` skips labels outside the clip rect, while `LabelSelectionState::on_end_pass` clears a
cross-label selection when either endpoint was not visited during the frame. The local patch keeps
offscreen selectable labels registered while a label selection exists. Painting remains clipped by
the surrounding UI, so only selection bookkeeping changes.

Remove this patch after upstream egui preserves cross-label selections whose endpoints are outside a
`ScrollArea` viewport.

Torto also exposes whether the vertical scroll bar is being interacted with so focus-mode scroll bar
input follows the same paragraph-based navigation path as mouse-wheel input. The upstream 0.36.2
`Sense::drag` hit-testing fix is included alongside these local changes.

The default image texture loader also evicts least recently used, inactive textures when its
retained pixel data exceeds 64 MiB. Textures touched in the current or previous pass are protected;
visible content can exceed this soft budget. This bounds unused UI image retention without
changing the texture format or the GPU device allocation strategy.

The merged upstream PR [#8316](https://github.com/emilk/egui/pull/8316) is backported to 0.36.2. It preserves automatic IDs through virtual spacers, assigns deterministic table/header/cell IDs (paired with the egui_extras patch), ignores existing widgets moving into vacated rectangles, and adds pass-local rectangle exclusions. Stable 0.36 uses `UiBuilder::id` and `Id::new` instead of the newer scope-ID names; the diagnostic regression tests are retained. The broader #8343 issue remains open, so Torto's diagnostic setting is not removed.
