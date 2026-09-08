pub mod application;
mod cli;
mod commands;
pub mod metrics;
pub mod render;
pub mod surface;
pub mod transition;

use std::env;
use std::process;

fn main() {
    let command = match cli::parse_args(env::args()) {
        Ok(cmd) => cmd,
        Err(err) => {
            eprintln!("Error: {err}");
            process::exit(1);
        }
    };

    let result = match command {
        cli::Command::Inspect { book } => commands::inspect::run(&book),
        cli::Command::Paginate {
            book,
            width,
            height,
            pages,
        } => commands::paginate::run(&book, width, height, pages),
        cli::Command::Render {
            book,
            output,
            width,
            height,
            metrics,
            metrics_file,
            profile,
        } => commands::render::run(
            &book,
            &output,
            width,
            height,
            metrics,
            metrics_file.as_deref(),
            profile,
        ),
        cli::Command::Window {
            book,
            metrics,
            metrics_file,
            profile,
            transition,
        } => commands::window::run(&book, metrics, metrics_file, profile, transition),
    };

    if let Err(err) = result {
        eprintln!("Error: {err}");
        process::exit(1);
    }
}
