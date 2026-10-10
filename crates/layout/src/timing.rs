//! Opt-in, thread-local preparation timings. Nested phases account for exclusive
//! wall time, so image work is not also charged to its enclosing layout phase.
use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingStage {
    Other,
    SectionParse,
    Fonts,
    ImageSource,
    ImageMetadata,
    ImageCache,
    ImageDecode,
    ImagePixels,
    Layout,
    GlyphShape,
    Hyphenation,
    LineBreak,
    Pagination,
    DisplayList,
}

const STAGES: usize = TimingStage::DisplayList as usize + 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct PreparationTimings {
    pub total: Duration,
    durations: [Duration; STAGES],
    calls: [usize; STAGES],
}

impl PreparationTimings {
    pub fn duration(&self, stage: TimingStage) -> Duration {
        self.durations[stage as usize]
    }

    pub fn calls(&self, stage: TimingStage) -> usize {
        self.calls[stage as usize]
    }
}

struct Account {
    id: u64,
    started: Instant,
    last: Instant,
    stage: TimingStage,
    timings: PreparationTimings,
}

impl Account {
    fn charge(&mut self, now: Instant) {
        self.timings.durations[self.stage as usize] += now.duration_since(self.last);
        self.last = now;
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Account>> = const { RefCell::new(None) };
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
}

/// Captures only this thread. Prefetch workers are deliberately excluded.
/// A nested capture returns `None` and leaves the existing capture intact.
pub struct TimingScope {
    id: u64,
    _thread: PhantomData<Rc<()>>,
}

impl TimingScope {
    pub fn start() -> Option<Self> {
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active.is_some() {
                return None;
            }
            let id = NEXT_ID.with(|next| {
                let id = next.get().wrapping_add(1);
                next.set(id);
                id
            });
            let now = Instant::now();
            *active = Some(Account {
                id,
                started: now,
                last: now,
                stage: TimingStage::Other,
                timings: PreparationTimings::default(),
            });
            Some(Self {
                id,
                _thread: PhantomData,
            })
        })
    }

    pub fn finish(self) -> PreparationTimings {
        ACTIVE.with(|active| {
            let mut account = active.borrow_mut().take().expect("active timing scope");
            debug_assert_eq!(account.id, self.id);
            let now = Instant::now();
            account.charge(now);
            account.timings.total = now.duration_since(account.started);
            account.timings
        })
    }
}

impl Drop for TimingScope {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active.as_ref().is_some_and(|account| account.id == self.id) {
                *active = None;
            }
        });
    }
}

pub struct TimingGuard {
    previous: Option<(u64, TimingStage)>,
    _thread: PhantomData<Rc<()>>,
}

/// A no-op outside an explicit capture; no logging or global locks.
pub fn stage(stage: TimingStage) -> TimingGuard {
    let previous = ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let account = active.as_mut()?;
        account.charge(Instant::now());
        account.timings.calls[stage as usize] += 1;
        Some((account.id, std::mem::replace(&mut account.stage, stage)))
    });
    TimingGuard {
        previous,
        _thread: PhantomData,
    }
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        let Some((id, previous)) = self.previous else {
            return;
        };
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if let Some(account) = active.as_mut().filter(|account| account.id == id) {
                account.charge(Instant::now());
                account.stage = previous;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_phases_have_exclusive_time_and_cover_the_whole_interval() {
        let start = Instant::now();
        let mut account = Account {
            id: 1,
            started: start,
            last: start,
            stage: TimingStage::Layout,
            timings: PreparationTimings::default(),
        };
        account.charge(start + Duration::from_millis(3));
        account.stage = TimingStage::ImageDecode;
        account.charge(start + Duration::from_millis(8));
        account.stage = TimingStage::Layout;
        account.charge(start + Duration::from_millis(10));
        assert_eq!(
            account.timings.duration(TimingStage::Layout),
            Duration::from_millis(5)
        );
        assert_eq!(
            account.timings.duration(TimingStage::ImageDecode),
            Duration::from_millis(5)
        );
        assert_eq!(
            account.timings.durations.iter().sum::<Duration>(),
            Duration::from_millis(10)
        );
    }

    #[test]
    fn background_threads_are_excluded_and_nested_capture_does_not_reset() {
        let scope = TimingScope::start().unwrap();
        assert!(TimingScope::start().is_none());
        std::thread::spawn(|| {
            let _guard = stage(TimingStage::ImageDecode);
        })
        .join()
        .unwrap();
        {
            let _layout = stage(TimingStage::Layout);
            let _decode = stage(TimingStage::ImageDecode);
        }
        let result = scope.finish();
        assert_eq!(result.calls(TimingStage::ImageDecode), 1);
        assert_eq!(result.calls(TimingStage::Layout), 1);
        assert_eq!(result.durations.iter().sum::<Duration>(), result.total);
    }

    #[test]
    fn abandoned_or_finished_scope_cannot_charge_a_later_capture() {
        let scope = TimingScope::start().unwrap();
        let guard = stage(TimingStage::Fonts);
        drop(scope);
        let next = TimingScope::start().unwrap();
        drop(guard);
        let result = next.finish();
        assert_eq!(result.calls(TimingStage::Fonts), 0);
        assert_eq!(result.duration(TimingStage::Fonts), Duration::ZERO);
    }
}
