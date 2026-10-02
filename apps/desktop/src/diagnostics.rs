#[derive(Clone, Copy)]
pub(crate) enum Field<'a> {
    Text(&'static str, &'static str),
    Detail(&'static str, &'a str),
    Bool(&'static str, bool),
    U64(&'static str, u64),
    Usize(&'static str, usize),
    F32(&'static str, f32),
}

mod imp {
    use std::fmt::Write as _;
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::panic;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Field;

    const MAX_LOG_BYTES: u64 = 1_048_576;
    static LOG_LOCK: Mutex<()> = Mutex::new(());

    pub(super) fn log(event: &'static str, fields: &[Field<'_>]) {
        // Release builds only write rare lifecycle/error events, never per-frame
        // reading diagnostics or request bodies.
        #[cfg(not(debug_assertions))]
        if !matches!(
            event,
            "app.start" | "app.exit" | "panic" | "render.fatal" | "window.minimize"
        ) {
            return;
        }
        let Ok(_guard) = LOG_LOCK.lock() else {
            return;
        };
        let Some(project) = crate::smoke::project_dirs() else {
            return;
        };
        let log_dir = project.data_local_dir().join("logs");
        if fs::create_dir_all(&log_dir).is_err() {
            return;
        }
        let path = log_dir.join(if cfg!(debug_assertions) {
            "reader-ui.log"
        } else {
            "runtime.log"
        });
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES)
            && fs::write(&path, []).is_err()
        {
            return;
        }
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut line = format!(
            "[{timestamp_ms}] event={event} pid={} version={}",
            std::process::id(),
            env!("CARGO_PKG_VERSION")
        );
        for field in fields {
            match *field {
                Field::Text(key, value) => {
                    let _ = write!(line, " {key}={}", value.replace(['\r', '\n', ' '], "_"));
                }
                Field::Detail(key, value) => {
                    let _ = write!(line, " {key}={value:?}");
                }
                Field::Bool(key, value) => {
                    let _ = write!(line, " {key}={value}");
                }
                Field::U64(key, value) => {
                    let _ = write!(line, " {key}={value}");
                }
                Field::Usize(key, value) => {
                    let _ = write!(line, " {key}={value}");
                }
                Field::F32(key, value) => {
                    let _ = write!(line, " {key}={value:.3}");
                }
            }
        }
        let _ = writeln!(file, "{line}");
        if event == "panic" {
            // Persist the crash record before abort/unwind can terminate the process.
            // Normal UI diagnostics do not pay the synchronous disk flush cost.
            let _ = file.sync_data();
        }
    }

    pub(super) fn install_panic_hook() {
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let message = info.to_string();
            let backtrace = std::backtrace::Backtrace::force_capture().to_string();
            let thread = std::thread::current();
            let thread_id = format!("{:?}", thread.id());
            if let Some(location) = info.location() {
                log(
                    "panic",
                    &[
                        Field::Text("location", "known"),
                        Field::Detail("file", location.file()),
                        Field::Detail("message", &message),
                        Field::Detail("backtrace", &backtrace),
                        Field::Detail("thread", thread.name().unwrap_or("unnamed")),
                        Field::Detail("thread_id", &thread_id),
                        Field::U64("line", u64::from(location.line())),
                        Field::U64("column", u64::from(location.column())),
                    ],
                );
            } else {
                log(
                    "panic",
                    &[
                        Field::Detail("message", &message),
                        Field::Detail("backtrace", &backtrace),
                        Field::Detail("thread", thread.name().unwrap_or("unnamed")),
                        Field::Detail("thread_id", &thread_id),
                    ],
                );
            }
            default_hook(info);
        }));
        let executable = std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        log("app.start", &[Field::Detail("executable", &executable)]);
    }
}

pub(crate) fn log(event: &'static str, fields: &[Field<'_>]) {
    imp::log(event, fields);
}

pub(crate) fn install_panic_hook() {
    imp::install_panic_hook();
}
