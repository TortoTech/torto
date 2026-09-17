//! Opt-in startup checks using the normal window, event loop and GPU renderer.
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static CHECK: OnceLock<Check> = OnceLock::new();
struct Check {
    output: PathBuf,
    started: Instant,
    expects_book: bool,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    ready_at: Option<Instant>,
    last_frame: Option<Instant>,
    frames: u64,
    completed: bool,
    gpu_error: Option<String>,
}

impl State {
    fn record_frame(&mut self, now: Instant, ready: bool) {
        if !ready
            || self
                .last_frame
                .is_some_and(|last| now.duration_since(last) > Duration::from_secs(2))
        {
            self.ready_at = None;
            self.frames = 0;
            self.completed = false;
        }
        self.last_frame = Some(now);
        if !ready {
            return;
        }
        let ready_at = *self.ready_at.get_or_insert(now);
        self.frames += 1;
        self.completed = self.frames >= 3 && now.duration_since(ready_at) >= Duration::from_secs(5);
    }
}

pub(crate) fn start(output: PathBuf, expects_book: bool) -> Result<(), String> {
    if !output.is_absolute() {
        return Err("smoke output directory must be absolute".into());
    }
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    // Refuse reused profiles: a fresh install is part of the test contract.
    std::fs::create_dir(output.join("profile")).map_err(|e| e.to_string())?;
    CHECK
        .set(Check {
            output,
            started: Instant::now(),
            expects_book,
            state: Mutex::new(State::default()),
        })
        .map_err(|_| "smoke check already initialized")?;
    std::fs::write(
        CHECK.get().unwrap().output.join("pid.json"),
        serde_json::json!({"pid":std::process::id()}).to_string(),
    )
    .map_err(|e| e.to_string())?;
    stage("startup");
    Ok(())
}

pub(crate) fn enabled() -> bool {
    CHECK.get().is_some()
}

pub(crate) fn project_dirs() -> Option<directories::ProjectDirs> {
    if let Some(check) = CHECK.get() {
        directories::ProjectDirs::from_path(check.output.join("profile"))
    } else {
        directories::ProjectDirs::from("com", "Rebook", "Rebook")
    }
}

pub(crate) fn stage(stage: &str) {
    if let Some(check) = CHECK.get() {
        eprintln!("TORTO_SMOKE {stage}");
        let _ = std::fs::write(check.output.join("stage.txt"), stage);
    }
}

pub(crate) fn gpu_error(error: String) {
    if let Some(check) = CHECK.get() {
        check.state.lock().unwrap().gpu_error = Some(error);
    }
}

pub(crate) fn presented(reader_ready: bool, error: Option<&str>) -> Result<(), String> {
    let Some(check) = CHECK.get() else {
        return Ok(());
    };
    let mut state = check.state.lock().unwrap();
    if let Some(error) = error.or(state.gpu_error.as_deref()) {
        return Err(error.into());
    }
    let now = Instant::now();
    let ready = !check.expects_book || reader_ready;
    if ready && state.ready_at.is_none() {
        stage("content-presented");
    }
    state.record_frame(now, ready);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_requires_ready_content_and_sustained_frames() {
        let start = Instant::now();
        let mut state = State::default();
        state.record_frame(start, false);
        state.record_frame(start + Duration::from_secs(8), false);
        assert!(!state.completed);
        state.record_frame(start + Duration::from_secs(9), true);
        state.record_frame(start + Duration::from_secs(10), true);
        assert!(!state.completed);
        for tick in 0..=50 {
            state.record_frame(
                start + Duration::from_secs(10) + Duration::from_millis(tick * 100),
                true,
            );
        }
        assert!(state.completed);
        state.record_frame(start + Duration::from_secs(20), true);
        assert!(
            !state.completed,
            "a long event-loop stall resets the observation window"
        );
        state.record_frame(start + Duration::from_secs(21), false);
        assert!(!state.completed);
        assert_eq!(state.frames, 0);
    }

    #[test]
    fn smoke_profile_paths_stay_inside_explicit_directory() {
        let root = std::env::temp_dir().join("torto-smoke-path-test");
        let dirs = directories::ProjectDirs::from_path(root.clone()).unwrap();
        for path in [
            dirs.config_dir(),
            dirs.data_dir(),
            dirs.data_local_dir(),
            dirs.cache_dir(),
        ] {
            assert!(
                path.starts_with(&root),
                "{} escaped {}",
                path.display(),
                root.display()
            );
        }
    }

    #[test]
    fn generated_publications_parse_and_layout() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let temp = std::env::temp_dir().join(format!("torto-fixture-{}", uuid::Uuid::new_v4()));
        let status = std::process::Command::new("node")
            .arg(root.join("scripts/smoke/fixtures.mjs"))
            .arg(&temp)
            .status()
            .unwrap();
        assert!(status.success());
        for name in ["startup.epub", "startup.pdf"] {
            let book = rebook_formats::open_file(temp.join(name)).unwrap();
            let source = book.source();
            let section = source.parse_section(0).unwrap();
            let layout = rebook_layout::LayoutEngine::new()
                .layout_section(
                    source.as_ref(),
                    &section,
                    rebook_layout::LayoutViewport::new(800, 700).unwrap(),
                    &rebook_layout::ReaderStyle::default(),
                )
                .unwrap();
            assert!(!layout.pages.is_empty());
            assert!(layout.pages.iter().any(|page| !page.items.is_empty()));
        }
        std::fs::remove_dir_all(temp).unwrap();
    }
}

pub(crate) fn should_exit() -> Result<bool, String> {
    let Some(check) = CHECK.get() else {
        return Ok(false);
    };
    let state = check.state.lock().unwrap();
    if let Some(error) = &state.gpu_error {
        return Err(error.clone());
    }
    if check.started.elapsed() > Duration::from_secs(60) {
        return Err("startup check timed out".into());
    }
    Ok(state.completed)
}

// Called after the event loop has returned; an early clean exit is still failure.
pub(crate) fn finish(error: Option<&str>) -> Result<(), String> {
    let Some(check) = CHECK.get() else {
        return Ok(());
    };
    let state = check.state.lock().unwrap();
    let error = error
        .or(state.gpu_error.as_deref())
        .or_else(|| (!state.completed).then_some("exited before startup check completed"));
    let report = serde_json::json!({
        "success": error.is_none(), "error": error, "pid": std::process::id(),
        "arch": std::env::consts::ARCH, "os": std::env::consts::OS,
        "book": check.expects_book, "frames": state.frames,
        "elapsed_ms": check.started.elapsed().as_millis(),
    });
    std::fs::write(
        check.output.join("result.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    error.map_or(Ok(()), |error| Err(error.into()))
}
