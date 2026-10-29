# Local egui patch

This directory vendors `egui 0.37.0` from crates.io.

The opt-in `native_text_input` bridge publishes the focused editable TextEdit's
complete text, character selection and geometry for Windows TSF. Native changes
are checked against the editor id and previous text, respect the field's limit,
and enter the normal undo history. Password, disabled and hidden fields are
excluded. TSF composition metadata uses the existing preedit painting while
avoiding a second insertion through legacy IME events. The bridge is dormant
unless explicitly enabled by the platform integration.

Torto's AI Chat allows a text selection to autoscroll beyond the visible viewport. Upstream
`Label::ui` skips labels outside the clip rect, while `LabelSelectionState::on_end_pass` clears a
cross-label selection when either endpoint was not visited during the frame. The local patch keeps
offscreen selectable labels registered while a label selection exists. Painting remains clipped by
the surrounding UI, so only selection bookkeeping changes.

Remove this patch after upstream egui preserves cross-label selections whose endpoints are outside a
`ScrollArea` viewport.

Torto also exposes whether the vertical scroll bar is being interacted with so focus-mode scroll bar
input follows the same paragraph-based navigation path as mouse-wheel input. The upstream scroll interaction behavior is otherwise retained. The local `State::set_offset`
method preserves scroll restoration and selection autoscroll now that the offset is private.

The default image texture loader also evicts least recently used, inactive textures when its
retained pixel data exceeds 64 MiB. Textures touched in the current or previous pass are protected;
visible content can exceed this soft budget. This bounds unused UI image retention without
changing the texture format or the GPU device allocation strategy.

The ID/exclusion/table fixes previously backported from upstream #8316 are now
included in 0.37.0; those local changes and the egui_extras override are retired.
Torto retains its diagnostic setting for issues outside that upstream fix.
