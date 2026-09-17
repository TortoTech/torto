#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Native e-book reader: parser -> reading IR -> page layout -> Vello scene -> egui/wgpu.

mod app;
mod async_task;
mod diagnostics;
mod fonts;
mod generated_metadata;
mod generated_toc;
mod highlights;
mod library;
mod persistence;
mod platform;
mod plugins;
mod preferences;
mod reader;
mod settings;
mod shelf;
mod smoke;
mod statistics;
mod sync;
mod ui;
#[cfg(target_os = "windows")]
mod updater;

use std::env;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use app::DesktopApp;
use library::LocalLibrary;

fn main() -> ExitCode {
    let result = run();
    let error = result.as_ref().err().map(ToString::to_string);
    let smoke_result = smoke::finish(error.as_deref());
    match result.and_then(|()| smoke_result.map_err(Into::into)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("torto failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let launch = parse_arguments()?;
    diagnostics::install_panic_hook();
    let reader_fonts = fonts::embedded_reader_fonts();

    let library =
        LocalLibrary::load_default().map_err(|error| io::Error::other(error.to_string()))?;
    let mut state = DesktopApp::new(library, Arc::clone(&reader_fonts));
    if let LaunchMode::Open(path) = launch {
        state.open_book(&path);
    }
    platform::run(state)
}

enum LaunchMode {
    Shelf,
    Open(PathBuf),
}

fn parse_arguments() -> Result<LaunchMode, Box<dyn std::error::Error>> {
    let mut arguments = env::args_os();
    let executable = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "torto".into());
    let Some(first) = arguments.next() else {
        return Ok(LaunchMode::Shelf);
    };
    if first == "--smoke-test" || first == "--smoke-test-open-event" {
        let expects_open_event = first == "--smoke-test-open-event";
        let output = arguments.next().ok_or("missing smoke output directory")?;
        let book = arguments.next().map(PathBuf::from);
        if arguments.next().is_some() {
            return Err(usage(&executable).into());
        }
        smoke::start(PathBuf::from(output), expects_open_event || book.is_some())?;
        return Ok(book.map_or(LaunchMode::Shelf, LaunchMode::Open));
    }
    let launch = LaunchMode::Open(PathBuf::from(first));
    if arguments.next().is_some() {
        return Err(usage(&executable).into());
    }
    Ok(launch)
}

fn usage(executable: &str) -> String {
    format!("usage: {executable} [book]")
}
