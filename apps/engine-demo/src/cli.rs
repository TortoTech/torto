use std::path::PathBuf;

use crate::metrics::MetricsFormat;
use crate::render::scene_cache::ResourceProfile;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Inspect {
        book: PathBuf,
    },
    Paginate {
        book: PathBuf,
        width: u32,
        height: u32,
        pages: Option<usize>,
    },
    Render {
        book: PathBuf,
        output: PathBuf,
        width: u32,
        height: u32,
        metrics: Option<MetricsFormat>,
        metrics_file: Option<PathBuf>,
        profile: ResourceProfile,
    },
    Window {
        book: PathBuf,
        metrics: Option<MetricsFormat>,
        metrics_file: Option<PathBuf>,
        profile: ResourceProfile,
        transition: Option<crate::transition::TransitionKind>,
    },
}

#[allow(clippy::too_many_lines)]
pub fn parse_args<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = String>,
{
    let mut iter = args.into_iter();
    let _bin_name = iter.next();

    let subcmd = iter.next().ok_or_else(|| {
        "No command provided. Usage: rebook-engine-demo <inspect|paginate|render|window> [OPTIONS] BOOK"
            .to_string()
    })?;

    match subcmd.as_str() {
        "inspect" => {
            let mut book = None;
            for arg in iter {
                if !arg.starts_with('-') && book.is_none() {
                    book = Some(PathBuf::from(arg));
                }
            }
            let book = book.ok_or_else(|| "Usage: rebook-engine-demo inspect BOOK".to_string())?;
            Ok(Command::Inspect { book })
        }
        "paginate" => {
            let mut book = None;
            let mut width = 800;
            let mut height = 1000;
            let mut pages = None;

            let mut iter = iter.peekable();
            while let Some(arg) = iter.next() {
                if arg == "--width" {
                    let val = iter.next().ok_or("Missing value for --width")?;
                    width = val
                        .parse::<u32>()
                        .map_err(|e| format!("Invalid width: {e}"))?;
                } else if arg == "--height" {
                    let val = iter.next().ok_or("Missing value for --height")?;
                    height = val
                        .parse::<u32>()
                        .map_err(|e| format!("Invalid height: {e}"))?;
                } else if arg == "--pages" {
                    let val = iter.next().ok_or("Missing value for --pages")?;
                    pages = Some(
                        val.parse::<usize>()
                            .map_err(|e| format!("Invalid pages: {e}"))?,
                    );
                } else if !arg.starts_with('-') && book.is_none() {
                    book = Some(PathBuf::from(arg));
                }
            }
            let book = book.ok_or_else(|| {
                "Usage: rebook-engine-demo paginate BOOK [--width W] [--height H] [--pages N]"
                    .to_string()
            })?;
            Ok(Command::Paginate {
                book,
                width,
                height,
                pages,
            })
        }
        "render" => {
            let mut book = None;
            let mut output = None;
            let mut width = 800;
            let mut height = 1000;
            let mut metrics = None;
            let mut metrics_file = None;
            let mut profile = ResourceProfile::Balanced;

            let mut iter = iter.peekable();
            while let Some(arg) = iter.next() {
                if arg == "--output" || arg == "-o" {
                    let val = iter.next().ok_or("Missing value for --output")?;
                    output = Some(PathBuf::from(val));
                } else if arg == "--width" {
                    let val = iter.next().ok_or("Missing value for --width")?;
                    width = val
                        .parse::<u32>()
                        .map_err(|e| format!("Invalid width: {e}"))?;
                } else if arg == "--height" {
                    let val = iter.next().ok_or("Missing value for --height")?;
                    height = val
                        .parse::<u32>()
                        .map_err(|e| format!("Invalid height: {e}"))?;
                } else if arg == "--metrics" {
                    let val = iter.next().ok_or("Missing value for --metrics")?;
                    match val.as_str() {
                        "text" => metrics = Some(MetricsFormat::Text),
                        "json" => metrics = Some(MetricsFormat::Json),
                        other => {
                            return Err(format!(
                                "Invalid --metrics format: {other}. Expected text or json"
                            ));
                        }
                    }
                } else if arg == "--metrics-file" {
                    let val = iter.next().ok_or("Missing value for --metrics-file")?;
                    metrics_file = Some(PathBuf::from(val));
                } else if arg == "--profile" {
                    let val = iter.next().ok_or("Missing value for --profile")?;
                    match val.to_lowercase().as_str() {
                        "low" => profile = ResourceProfile::Low,
                        "balanced" => profile = ResourceProfile::Balanced,
                        "high" => profile = ResourceProfile::High,
                        other => {
                            return Err(format!(
                                "Invalid --profile: {other}. Expected low, balanced, or high"
                            ));
                        }
                    }
                } else if !arg.starts_with('-') && book.is_none() {
                    book = Some(PathBuf::from(arg));
                }
            }
            let book = book.ok_or_else(|| {
                "Usage: rebook-engine-demo render BOOK --output PAGE.png [--metrics text|json] [--metrics-file PATH] [--profile low|balanced|high]".to_string()
            })?;
            let output = output.ok_or_else(|| {
                "Missing required --output parameter for render command".to_string()
            })?;
            Ok(Command::Render {
                book,
                output,
                width,
                height,
                metrics,
                metrics_file,
                profile,
            })
        }
        "window" => {
            let mut book = None;
            let mut metrics = None;
            let mut metrics_file = None;
            let mut profile = ResourceProfile::Balanced;
            let mut transition = None;

            let mut iter = iter.peekable();
            while let Some(arg) = iter.next() {
                if arg == "--metrics" {
                    let val = iter.next().ok_or("Missing value for --metrics")?;
                    match val.as_str() {
                        "text" => metrics = Some(MetricsFormat::Text),
                        "json" => metrics = Some(MetricsFormat::Json),
                        other => {
                            return Err(format!(
                                "Invalid --metrics format: {other}. Expected text or json"
                            ));
                        }
                    }
                } else if arg == "--metrics-file" {
                    let val = iter.next().ok_or("Missing value for --metrics-file")?;
                    metrics_file = Some(PathBuf::from(val));
                } else if arg == "--profile" {
                    let val = iter.next().ok_or("Missing value for --profile")?;
                    match val.to_lowercase().as_str() {
                        "low" => profile = ResourceProfile::Low,
                        "balanced" => profile = ResourceProfile::Balanced,
                        "high" => profile = ResourceProfile::High,
                        other => {
                            return Err(format!(
                                "Invalid --profile: {other}. Expected low, balanced, or high"
                            ));
                        }
                    }
                } else if arg == "--transition" {
                    let val = iter.next().ok_or("Missing value for --transition")?;
                    match val.to_lowercase().as_str() {
                        "none" => transition = Some(crate::transition::TransitionKind::None),
                        "slide" => transition = Some(crate::transition::TransitionKind::Slide),
                        other => {
                            return Err(format!(
                                "Invalid --transition: {other}. Expected none or slide"
                            ));
                        }
                    }
                } else if !arg.starts_with('-') && book.is_none() {
                    book = Some(PathBuf::from(arg));
                }
            }
            let book = book.ok_or_else(|| "Usage: rebook-engine-demo window BOOK [--metrics text|json] [--metrics-file PATH] [--profile low|balanced|high] [--transition none|slide]".to_string())?;
            Ok(Command::Window {
                book,
                metrics,
                metrics_file,
                profile,
                transition,
            })
        }
        other => Err(format!(
            "Unknown command: {other}. Expected inspect, paginate, render, or window."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inspect_command() {
        let args = vec![
            "rebook-engine-demo".into(),
            "inspect".into(),
            "book.epub".into(),
        ];
        assert_eq!(
            parse_args(args).unwrap(),
            Command::Inspect {
                book: PathBuf::from("book.epub"),
            }
        );
    }

    #[test]
    fn parses_paginate_command_with_defaults_and_options() {
        let args = vec![
            "rebook-engine-demo".into(),
            "paginate".into(),
            "book.epub".into(),
            "--width".into(),
            "1024".into(),
            "--height".into(),
            "768".into(),
            "--pages".into(),
            "5".into(),
        ];
        assert_eq!(
            parse_args(args).unwrap(),
            Command::Paginate {
                book: PathBuf::from("book.epub"),
                width: 1024,
                height: 768,
                pages: Some(5),
            }
        );
    }

    #[test]
    fn parses_render_and_window_commands_with_metrics_and_profile() {
        let render_args = vec![
            "rebook-engine-demo".into(),
            "render".into(),
            "book.epub".into(),
            "--output".into(),
            "page.png".into(),
            "--metrics".into(),
            "json".into(),
            "--profile".into(),
            "high".into(),
        ];
        assert_eq!(
            parse_args(render_args).unwrap(),
            Command::Render {
                book: PathBuf::from("book.epub"),
                output: PathBuf::from("page.png"),
                width: 800,
                height: 1000,
                metrics: Some(MetricsFormat::Json),
                metrics_file: None,
                profile: ResourceProfile::High,
            }
        );

        let window_args = vec![
            "rebook-engine-demo".into(),
            "window".into(),
            "book.epub".into(),
            "--metrics".into(),
            "text".into(),
            "--metrics-file".into(),
            "/tmp/metrics.txt".into(),
            "--transition".into(),
            "slide".into(),
        ];
        assert_eq!(
            parse_args(window_args).unwrap(),
            Command::Window {
                book: PathBuf::from("book.epub"),
                metrics: Some(MetricsFormat::Text),
                metrics_file: Some(PathBuf::from("/tmp/metrics.txt")),
                profile: ResourceProfile::Balanced,
                transition: Some(crate::transition::TransitionKind::Slide),
            }
        );
    }
}
