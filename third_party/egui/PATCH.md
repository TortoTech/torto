# Local egui patch

This directory vendors `egui 0.36.1` from crates.io.

Torto's AI Chat allows a text selection to autoscroll beyond the visible viewport. Upstream
`Label::ui` skips labels outside the clip rect, while `LabelSelectionState::on_end_pass` clears a
cross-label selection when either endpoint was not visited during the frame. The local patch keeps
offscreen selectable labels registered while a label selection exists. Painting remains clipped by
the surrounding UI, so only selection bookkeeping changes.

Remove this patch after upstream egui preserves cross-label selections whose endpoints are outside a
`ScrollArea` viewport.

Torto also exposes whether the vertical scroll bar is being interacted with so focus-mode scroll bar
input follows the same paragraph-based navigation path as mouse-wheel input. The upstream 0.36.1
`Sense::drag` hit-testing fix is included alongside these local changes.
