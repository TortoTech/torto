use std::time::Instant;

#[derive(Clone, Copy)]
struct EguiDeadline {
    when: Instant,
    pass: u64,
}

/// egui callbacks describe a particular pass. Background notifications do not:
/// they remain valid even if unrelated UI work has rendered many more passes.
#[derive(Default)]
pub(super) struct RepaintSchedule {
    egui: Vec<EguiDeadline>,
    external: Option<Instant>,
    redraw_pending: bool,
}

fn pass_is_current(request: u64, current: u64) -> bool {
    request == current || request.checked_add(1) == Some(current)
}

impl RepaintSchedule {
    pub fn egui(&mut self, when: Instant, pass: u64, current: u64) {
        self.prune(current);
        if !pass_is_current(pass, current) {
            return;
        }
        if let Some(pending) = self.egui.iter_mut().find(|pending| pending.pass == pass) {
            pending.when = pending.when.min(when);
        } else {
            self.egui.push(EguiDeadline { when, pass });
        }
    }

    pub fn external(&mut self, when: Instant) {
        self.external = Some(self.external.map_or(when, |pending| pending.min(when)));
    }

    fn prune(&mut self, current: u64) {
        self.egui
            .retain(|pending| pass_is_current(pending.pass, current));
    }

    pub fn next_deadline(&mut self, current: u64) -> Option<Instant> {
        self.prune(current);
        self.egui
            .iter()
            .map(|pending| pending.when)
            .chain(self.external)
            .min()
    }

    fn consume_due(&mut self, now: Instant, current: u64) -> bool {
        self.prune(current);
        let due = self.external.is_some_and(|when| when <= now)
            || self.egui.iter().any(|pending| pending.when <= now);
        if self.external.is_some_and(|when| when <= now) {
            self.external = None;
        }
        self.egui.retain(|pending| pending.when > now);
        due
    }

    pub fn take_due(&mut self, now: Instant, current: u64) -> bool {
        if self.consume_due(now, current) && !self.redraw_pending {
            self.redraw_pending = true;
            true
        } else {
            false
        }
    }

    /// A native redraw also services any deadlines that have already expired.
    pub fn on_redraw(&mut self, now: Instant, current: u64) {
        self.consume_due(now, current);
        self.redraw_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn accepts_current_and_previous_pass_and_rejects_stale_or_future() {
        let now = Instant::now();
        for (pass, accepted) in [(8, false), (9, true), (10, true), (11, false)] {
            let mut schedule = RepaintSchedule::default();
            schedule.egui(now, pass, 10);
            assert_eq!(schedule.take_due(now, 10), accepted);
        }
        assert!(!pass_is_current(u64::MAX, 0));
    }

    #[test]
    fn absolute_deadline_is_not_extended_by_event_queue_delay() {
        let sent = Instant::now();
        let deadline = sent + Duration::from_millis(30);
        let received = sent + Duration::from_millis(50);
        let mut schedule = RepaintSchedule::default();
        schedule.egui(deadline, 1, 1);
        assert!(schedule.take_due(received, 1));
    }

    #[test]
    fn duplicate_requests_coalesce_and_native_redraw_consumes_due() {
        let now = Instant::now();
        let mut schedule = RepaintSchedule::default();
        schedule.egui(now + Duration::from_secs(2), 1, 1);
        schedule.egui(now + Duration::from_secs(1), 1, 1);
        assert_eq!(
            schedule.next_deadline(1),
            Some(now + Duration::from_secs(1))
        );
        assert!(schedule.take_due(now + Duration::from_secs(1), 1));
        schedule.egui(now, 1, 1);
        assert!(!schedule.take_due(now + Duration::from_secs(1), 1));
        schedule.on_redraw(now + Duration::from_secs(1), 1);
        assert_eq!(schedule.next_deadline(1), None);
        schedule.external(now);
        assert!(schedule.take_due(now + Duration::from_secs(1), 1));
    }

    #[test]
    fn stale_timer_is_pruned_but_newer_pass_deadline_survives() {
        let now = Instant::now();
        let mut schedule = RepaintSchedule::default();
        schedule.egui(now, 1, 2);
        let later = now + Duration::from_secs(3);
        schedule.egui(later, 2, 2);
        assert!(!schedule.take_due(now, 3));
        assert_eq!(schedule.next_deadline(3), Some(later));
        assert!(schedule.take_due(later, 3));
        assert_eq!(schedule.next_deadline(3), None);
    }

    #[test]
    fn external_notification_survives_unrelated_passes_and_merges_deadlines() {
        let now = Instant::now();
        let mut schedule = RepaintSchedule::default();
        schedule.external(now + Duration::from_secs(2));
        schedule.external(now + Duration::from_secs(3));
        schedule.egui(now + Duration::from_secs(1), 1, 1);
        assert_eq!(
            schedule.next_deadline(1),
            Some(now + Duration::from_secs(1))
        );
        assert_eq!(
            schedule.next_deadline(100),
            Some(now + Duration::from_secs(2))
        );
        assert!(schedule.take_due(now + Duration::from_secs(2), 100));
        assert!(!schedule.take_due(now + Duration::from_secs(3), 100));
        assert_eq!(schedule.next_deadline(100), None);
    }
}
