use super::*;
use rebook_publication::{SourceAnchor, SpineItemId};
fn source(node: &str) -> SourceRange {
    let start = SourceAnchor {
        spine: SpineItemId::new("chapter").unwrap(),
        node: node.into(),
        text_offset: 0,
    };
    SourceRange {
        start: start.clone(),
        end: start,
    }
}
fn task(key: &str, kind: Kind, node: &str) -> Task {
    Task {
        key: key.into(),
        kind,
        section: 0,
        sources: vec![source(node)],
        prefetch: node != "a",
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}
#[test]
fn hosts_share_phase_dependencies_prefetch_and_failure_policy() {
    assert_eq!(
        Flow {
            structure: true,
            translation: true
        }
        .stages()
        .collect::<Vec<_>>(),
        vec![Kind::Structure, Kind::Translation]
    );
    assert!(
        Flow {
            structure: true,
            translation: true
        }
        .translation_ready(true)
    );
    assert!(
        !Flow {
            structure: true,
            translation: true
        }
        .translation_ready(false)
    );
    assert!(
        !Flow {
            structure: true,
            translation: true
        }
        .presentation_ready(false)
    );
    assert_eq!(
        Flow::scope(true, true, false, true)
            .stages()
            .collect::<Vec<_>>(),
        vec![Kind::Structure]
    );
    assert!(
        Flow::scope(true, true, false, false)
            .stages()
            .next()
            .is_none()
    );
    assert!(matches!(
        failure(Kind::Structure, "transport failed"),
        Failure::KeepOriginal
    ));
    assert!(matches!(
        failure(Kind::Translation, "transport failed"),
        Failure::StopBatch
    ));
    assert!(matches!(
        failure(Kind::Translation, "AI request superseded"),
        Failure::Interrupted
    ));
}
#[test]
fn every_kind_uses_the_same_execution_timeout_and_releases_its_lane() {
    runtime().block_on(async {
        for kind in [
            Kind::Structure,
            Kind::Translation,
            Kind::Enhancement,
            Kind::Contents,
        ] {
            let scheduler = Scheduler(Arc::new(Inner {
                timeout: Duration::from_millis(5),
                ..Default::default()
            }));
            let result = scheduler
                .run(
                    task("timeout", kind, "a"),
                    &Default::default(),
                    std::future::pending::<Result<(), String>>(),
                )
                .await;
            assert_eq!(result.unwrap_err(), "AI request timed out");
            assert!(scheduler.0.state.lock().unwrap().entries.is_empty());
        }
    });
}
#[test]
fn ready_body_overtakes_contents_and_prefetch_and_duplicate_work_is_coalesced() {
    runtime().block_on(async {
        let scheduler = Scheduler::default();
        scheduler.update(vec![(0, vec![source("a")])], true);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut workers = vec![];
        for (key, kind, node) in [
            ("prefetch", Kind::Translation, "b"),
            ("contents", Kind::Contents, ""),
            ("visible", Kind::Translation, "a"),
        ] {
            let scheduler = scheduler.clone();
            let tx = tx.clone();
            workers.push(tokio::spawn(async move {
                scheduler
                    .run(task(key, kind, node), &Default::default(), async move {
                        tx.send(key).unwrap();
                        Ok(())
                    })
                    .await
            }));
        }
        while scheduler.0.state.lock().unwrap().entries.len() != 3 {
            tokio::task::yield_now().await;
        }
        assert!(
            scheduler
                .run(
                    task("visible", Kind::Translation, "a"),
                    &Default::default(),
                    async { Ok(()) }
                )
                .await
                .unwrap_err()
                .contains("duplicate")
        );
        scheduler.update(vec![(0, vec![source("a")])], false);
        assert_eq!(rx.recv().await, Some("visible"));
        assert_eq!(rx.recv().await, Some("contents"));
        assert_eq!(rx.recv().await, Some("prefetch"));
        for worker in workers {
            worker.await.unwrap().unwrap();
        }
        assert!(scheduler.0.state.lock().unwrap().entries.is_empty());
    });
}
#[test]
fn moving_offscreen_preserves_paid_work_until_new_visible_work_needs_the_lane() {
    runtime().block_on(async {
        let scheduler = Scheduler::default();
        scheduler.update(vec![(0, vec![source("a")])], false);
        let (started, ready) = tokio::sync::oneshot::channel();
        let s = scheduler.clone();
        let old = tokio::spawn(async move {
            s.run(
                task("old", Kind::Translation, "a"),
                &Default::default(),
                async {
                    let _ = started.send(());
                    std::future::pending::<Result<(), String>>().await
                },
            )
            .await
        });
        ready.await.unwrap();
        scheduler.update(vec![(0, vec![source("b")])], false);
        tokio::task::yield_now().await;
        assert!(!old.is_finished());
        let s = scheduler.clone();
        let new = tokio::spawn(async move {
            s.run(
                task("new", Kind::Translation, "b"),
                &Default::default(),
                async { Ok(()) },
            )
            .await
        });
        // A transient viewport must settle before it interrupts a paid request.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!old.is_finished());
        new.await.unwrap().unwrap();
        assert!(interrupted(&old.await.unwrap().unwrap_err()));
        assert!(scheduler.0.state.lock().unwrap().entries.is_empty());
    });
}
#[test]
fn cancellation_invalidation_and_abandoned_futures_release_every_slot() {
    runtime().block_on(async {
        let scheduler = Scheduler::default();
        scheduler.pause();
        let token = Cancellation::default();
        let s = scheduler.clone();
        let t = token.clone();
        let queued = tokio::spawn(async move {
            s.run(task("queued", Kind::Structure, "a"), &t, async { Ok(()) })
                .await
        });
        while scheduler.0.state.lock().unwrap().entries.is_empty() {
            tokio::task::yield_now().await;
        }
        token.cancel();
        assert!(interrupted(&queued.await.unwrap().unwrap_err()));
        let s = scheduler.clone();
        let abandoned = tokio::spawn(async move {
            s.run(
                task("abandoned", Kind::Structure, "a"),
                &Default::default(),
                async { Ok(()) },
            )
            .await
        });
        while scheduler.0.state.lock().unwrap().entries.is_empty() {
            tokio::task::yield_now().await;
        }
        abandoned.abort();
        let _ = abandoned.await;
        assert!(scheduler.0.state.lock().unwrap().entries.is_empty());
        let s = scheduler.clone();
        let invalidated = tokio::spawn(async move {
            s.run(
                task("epoch", Kind::Structure, "a"),
                &Default::default(),
                async { Ok(()) },
            )
            .await
        });
        while scheduler.0.state.lock().unwrap().entries.is_empty() {
            tokio::task::yield_now().await;
        }
        scheduler.invalidate();
        assert!(interrupted(&invalidated.await.unwrap().unwrap_err()));
        scheduler.update(vec![(0, vec![source("a")])], false);
        scheduler
            .run(
                task("fresh", Kind::Structure, "a"),
                &Default::default(),
                async { Ok(()) },
            )
            .await
            .unwrap();
    });
}
#[test]
fn queues_remain_bounded_and_visible_work_can_replace_low_priority_backlog() {
    runtime().block_on(async {
        let scheduler = Scheduler::default();
        scheduler.update(vec![(0, vec![source("a")])], true);
        let mut workers = vec![];
        for index in 0..MAX_QUEUED {
            let s = scheduler.clone();
            workers.push(tokio::spawn(async move {
                s.run(
                    task(&format!("p{index}"), Kind::Structure, "b"),
                    &Default::default(),
                    async { Ok(()) },
                )
                .await
            }));
        }
        while scheduler.0.state.lock().unwrap().entries.len() < MAX_QUEUED {
            tokio::task::yield_now().await;
        }
        let s = scheduler.clone();
        let visible = tokio::spawn(async move {
            s.run(
                task("important", Kind::Structure, "a"),
                &Default::default(),
                async { Ok(()) },
            )
            .await
        });
        while !scheduler
            .0
            .state
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|e| e.task.key == "important")
        {
            tokio::task::yield_now().await;
        }
        assert_eq!(scheduler.0.state.lock().unwrap().entries.len(), MAX_QUEUED);
        scheduler.update(vec![(0, vec![source("a")])], false);
        visible.await.unwrap().unwrap();
        scheduler.invalidate();
        for worker in workers {
            let _ = worker.await;
        }
        assert!(scheduler.0.state.lock().unwrap().entries.is_empty());
    });
}
