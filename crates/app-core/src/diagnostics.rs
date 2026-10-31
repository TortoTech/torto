#[derive(Clone, Copy)]
pub enum Field<'a> {
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
            "app.start"
                | "app.exit"
                | "panic"
                | "render.fatal"
                | "pdf.agent.finished"
                | "pdf.native.converted"
                | "window.minimize"
                | "memory.caches"
                | "memory.gpu"
                | "memory.gpu_device"
                | "memory.gpu_trim"
                | "memory.rust"
                | "memory.probe"
        ) {
            return;
        }
        let Ok(_guard) = LOG_LOCK.lock() else {
            return;
        };
        let Some(project) = crate::environment::paths() else {
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

pub fn log(event: &'static str, fields: &[Field<'_>]) {
    #[cfg(any(test, feature = "test-support"))]
    if open_capture::record(event, fields) {
        return;
    }
    imp::log(event, fields);
}

pub fn reader_timings_enabled() -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if open_capture::active() {
        return true;
    }
    cfg!(debug_assertions)
}

#[cfg(any(test, feature = "test-support"))]
pub mod open_capture {
    use super::Field;
    use serde_json::{Value, json};
    use std::cell::RefCell;
    thread_local! {
        static EVENTS: RefCell<Option<Vec<Value>>> = const { RefCell::new(None) };
    }
    pub fn start() {
        EVENTS.with(|e| *e.borrow_mut() = Some(Vec::new()));
    }
    pub fn active() -> bool {
        EVENTS.with(|e| e.borrow().is_some())
    }
    pub fn finish() -> Vec<Value> {
        EVENTS.with(|e| e.borrow_mut().take().unwrap())
    }
    pub(super) fn record(event: &str, fields: &[Field<'_>]) -> bool {
        EVENTS.with(|e| {
            let mut values = e.borrow_mut();
            let Some(values) = values.as_mut() else {
                return false;
            };
            if event.starts_with("reader.open") || event == "reader.visible_images_ready" {
                let mut value = serde_json::Map::new();
                value.insert("event".into(), json!(event));
                for field in fields {
                    let (key, entry) = match field {
                        Field::Text(k, v) => (*k, json!(v)),
                        Field::Detail(k, v) => (*k, json!(v)),
                        Field::Bool(k, v) => (*k, json!(v)),
                        Field::U64(k, v) => (*k, json!(v)),
                        Field::Usize(k, v) => (*k, json!(v)),
                        Field::F32(k, v) => (*k, json!(v)),
                    };
                    value.insert(key.into(), entry);
                }
                values.push(Value::Object(value));
            }
            true // Profiling logs stay in this capture, never the user's log file.
        })
    }
}

pub fn install_panic_hook() {
    imp::install_panic_hook();
}
