//! Keep sync/reopen callbacks from presenting the same remote progress again.
use std::collections::VecDeque;

use super::ResumeOffer;

const HISTORY_LIMIT: usize = 16;

#[derive(Default)]
/// Bounded process-local history shared across reader reopens by each host.
pub struct ResumeNoticeHistory {
    shown: VecDeque<(String, ResumeOffer)>,
}

impl ResumeNoticeHistory {
    pub fn contains(&self, account: &str, offer: &ResumeOffer) -> bool {
        self.shown.iter().any(|(seen_account, seen)| {
            seen_account == account
                && seen.book_id == offer.book_id
                && seen.remote.updated_at >= offer.remote.updated_at
        })
    }

    pub fn mark_shown(&mut self, account: &str, offer: &ResumeOffer) {
        if self.contains(account, offer) {
            return;
        }
        self.shown.retain(|(seen_account, seen)| {
            seen_account != account || seen.book_id != offer.book_id
        });
        self.shown.push_back((account.to_owned(), offer.clone()));
        while self.shown.len() > HISTORY_LIMIT {
            self.shown.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(book: &str, time: u64, counter: u32) -> ResumeOffer {
        let locator = serde_json::json!({
            "version": 1,
            "publication_id": book,
            "href": {"path": "chapter.xhtml", "fragment": null},
            "total_progression": 0.8,
        });
        let mut baseline = locator.clone();
        baseline["total_progression"] = serde_json::json!(0.2);
        serde_json::from_value(serde_json::json!({
            "book_id": book,
            "baseline": baseline,
            "remote": {
                "locator": locator,
                "updated_at": {
                    "wall_time_ms": time,
                    "counter": counter,
                    "device_id": "peer",
                },
            },
        }))
        .unwrap()
    }

    #[test]
    fn same_remote_is_not_reoffered_after_jump_reflow_or_dismissal() {
        let mut history = ResumeNoticeHistory::default();
        let original = offer("book", 100, 0);
        assert!(!history.contains("account", &original));
        history.mark_shown("account", &original);

        // Focus/pagination can produce a different local locator after jumping.
        let mut reopened = original.clone();
        reopened.baseline.total_progression = Some(0.79);
        reopened.baseline.progression = Some(0.2);
        assert!(history.contains("account", &reopened));

        // Repeated sync completions must not restart the toast's lifetime.
        for _ in 0..20 {
            history.mark_shown("account", &reopened);
            assert!(history.contains("account", &original));
        }
        assert_eq!(history.shown.len(), 1);
    }

    #[test]
    fn newer_remote_progress_is_offered_and_delayed_older_callbacks_are_ignored() {
        let mut history = ResumeNoticeHistory::default();
        let old = offer("book", 100, 0);
        history.mark_shown("account", &old);
        let newer = offer("book", 100, 1);
        assert!(!history.contains("account", &newer));
        history.mark_shown("account", &newer);
        assert!(history.contains("account", &old));
        assert!(history.contains("account", &newer));
        assert!(!history.contains("account", &offer("book", 101, 0)));
        assert_eq!(history.shown.len(), 1);
    }

    #[test]
    fn notices_are_scoped_to_account_and_book_and_history_is_bounded() {
        let mut history = ResumeNoticeHistory::default();
        let original = offer("book", 100, 0);
        history.mark_shown("account", &original);
        assert!(!history.contains("other-account", &original));
        assert!(!history.contains("account", &offer("other-book", 100, 0)));
        for i in 0..HISTORY_LIMIT {
            history.mark_shown("account", &offer(&format!("book-{i}"), 100, 0));
        }
        assert_eq!(history.shown.len(), HISTORY_LIMIT);
        assert!(!history.contains("account", &original));
        assert!(history.contains("account", &offer(&format!("book-{}", HISTORY_LIMIT - 1), 100, 0)));
    }
}
