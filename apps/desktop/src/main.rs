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
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("torto failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    diagnostics::install_panic_hook();
    let launch = parse_arguments()?;
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
    let launch = LaunchMode::Open(PathBuf::from(first));
    if arguments.next().is_some() {
        return Err(usage(&executable).into());
    }
    Ok(launch)
}

fn usage(executable: &str) -> String {
    format!("usage: {executable} [book]")
}
