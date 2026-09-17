#[derive(Clone, Copy)]
pub(crate) enum Field {
    Text(&'static str, &'static str),
    Bool(&'static str, bool),
    U64(&'static str, u64),
    Usize(&'static str, usize),
    F32(&'static str, f32),
}

#[cfg(debug_assertions)]
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

    pub(super) fn log(event: &'static str, fields: &[Field]) {
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
        let path = log_dir.join("reader-ui.log");
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
        let mut line = format!("[{timestamp_ms}] event={event}");
        for field in fields {
            match *field {
                Field::Text(key, value) => {
                    let _ = write!(line, " {key}={}", value.replace(['\r', '\n', ' '], "_"));
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
    }

    pub(super) fn install_panic_hook() {
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if let Some(location) = info.location() {
                log(
                    "panic",
                    &[
                        Field::Text("location", "known"),
                        Field::U64("line", u64::from(location.line())),
                        Field::U64("column", u64::from(location.column())),
                    ],
                );
            } else {
                log("panic", &[]);
            }
            default_hook(info);
        }));
        log("app.start", &[]);
    }
}

#[cfg(not(debug_assertions))]
mod imp {
    use super::Field;

    pub(super) fn log(_event: &'static str, fields: &[Field]) {
        for field in fields {
            match *field {
                Field::Text(key, value) => {
                    let _ = (key, value);
                }
                Field::Bool(key, value) => {
                    let _ = (key, value);
                }
                Field::U64(key, value) => {
                    let _ = (key, value);
                }
                Field::Usize(key, value) => {
                    let _ = (key, value);
                }
                Field::F32(key, value) => {
                    let _ = (key, value);
                }
            }
        }
    }

    pub(super) fn install_panic_hook() {}
}

pub(crate) fn log(event: &'static str, fields: &[Field]) {
    imp::log(event, fields);
}

pub(crate) fn install_panic_hook() {
    imp::install_panic_hook();
}
