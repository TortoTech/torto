//! Opt-in full-text bridge for native text services. Only the focused editable
//! non-password TextEdit publishes a snapshot. Requests are scoped to its id and
//! exact previous text, so delayed services cannot overwrite another editor.
use crate::{Context, Id, Rect, text::CCursorRange};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub id: Id,
    pub text: Arc<str>,
    pub selection: CCursorRange,
    pub rect: Rect,
    pub cursor_rect: Rect,
    pub multiline: bool,
    pub char_limit: usize,
}

#[derive(Clone, Debug)]
pub struct Edit {
    pub id: Id,
    pub before: Arc<str>,
    pub text: Arc<str>,
    pub selection: CCursorRange,
    pub composition: Option<core::ops::Range<usize>>,
}

#[derive(Clone, Default)]
struct Bridge {
    enabled: bool,
    owner: Option<Id>,
    composing: bool,
    snapshot: Option<Snapshot>,
    edit: Option<Edit>,
    composition: Option<(Id, core::ops::Range<usize>)>,
}
fn key() -> Id {
    Id::unique("egui-native-text-input")
}
pub fn begin_frame(ctx: &Context, enabled: bool, owner: Option<Id>, edit: Option<Edit>) {
    ctx.data_mut(|d| {
        let bridge = d.get_temp_mut_or_default::<Bridge>(key());
        bridge.enabled = enabled;
        if bridge.owner != owner {
            bridge.composition = None;
        }
        bridge.owner = owner;
        bridge.snapshot = None;
        bridge.edit = edit;
        if !enabled {
            bridge.composition = None;
        }
    });
}
/// Update composition state without fabricating IME input events (which would
/// otherwise request another frame or reach a newly focused legacy editor).
pub fn set_composing(ctx: &Context, composing: bool) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Bridge>(key()).composing = composing);
}
/// Includes the final composition frame, before TextEdit applies its commit.
pub fn blocks_submit(ctx: &Context) -> bool {
    ctx.data(|d| {
        d.get_temp::<Bridge>(key()).is_some_and(|b| {
            b.enabled && b.owner.is_some() && (b.composing || b.composition.is_some())
        })
    })
}
pub fn snapshot(ctx: &Context) -> Option<Snapshot> {
    ctx.data(|d| d.get_temp::<Bridge>(key()).and_then(|b| b.snapshot))
}
pub(crate) fn publish(ctx: &Context, snapshot: Snapshot) {
    ctx.data_mut(|d| {
        let bridge = d.get_temp_mut_or_default::<Bridge>(key());
        if bridge.enabled {
            bridge.snapshot = Some(snapshot);
        }
    });
}
pub(crate) fn take_edit(ctx: &Context, id: Id) -> Option<Edit> {
    ctx.data_mut(|d| {
        let bridge = d.get_temp_mut_or_default::<Bridge>(key());
        if bridge.enabled && bridge.edit.as_ref().is_some_and(|e| e.id == id) {
            bridge.edit.take()
        } else {
            None
        }
    })
}
pub(crate) fn set_composition(ctx: &Context, id: Id, range: Option<core::ops::Range<usize>>) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Bridge>(key()).composition = range.map(|r| (id, r))
    });
}
pub fn composition(ctx: &Context, id: Id) -> Option<core::ops::Range<usize>> {
    ctx.data(|d| {
        d.get_temp::<Bridge>(key())
            .and_then(|b| b.composition)
            .filter(|(owner, _)| *owner == id)
            .map(|(_, r)| r)
    })
}
pub(crate) fn enabled(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<Bridge>(key()).is_some_and(|b| b.enabled))
}
pub(crate) fn handles_ime(ctx: &Context, _id: Id) -> bool {
    ctx.data(|d| {
        d.get_temp::<Bridge>(key())
            .is_some_and(|b| b.enabled && b.owner.is_some())
    })
}
