pub(crate) use rebook_app_core::diagnostics::*;
#[cfg(all(target_os = "windows", feature = "memory-profiling"))]
pub(crate) fn memory_checkpoint(stage: &'static str) {
    let (working, private) = process_memory();
    let (live, peak) = rebook_windows_window_background::rust_allocation_bytes();
    log(
        "memory.probe",
        &[
            Field::Text("stage", stage),
            Field::Usize("working_set_bytes", working),
            Field::Usize("private_bytes", private),
            Field::Usize("rust_live_bytes", live),
            Field::Usize("rust_peak_bytes", peak),
        ],
    );
    println!(
        "{stage}: working={:.2} MiB private={:.2} MiB rust_live={:.2} MiB rust_peak={:.2} MiB",
        working as f64 / 1_048_576.0,
        private as f64 / 1_048_576.0,
        live as f64 / 1_048_576.0,
        peak as f64 / 1_048_576.0
    );
}

/// Process counters stay separate from estimated cache/GPU allocations.
#[cfg(target_os = "windows")]
pub(crate) fn process_memory() -> (usize, usize) {
    rebook_windows_window_background::process_memory().unwrap_or_default()
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn process_memory() -> (usize, usize) {
    (0, 0)
}
