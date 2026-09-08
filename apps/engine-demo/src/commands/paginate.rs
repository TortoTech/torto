use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rebook_engine::{Engine, ReaderConfig};
use rebook_layout::{LayoutViewport, ReaderStyle};
use rebook_reader::{NavigationAttempt, PageDirection};

#[allow(clippy::too_many_lines)]
pub fn run(
    book_path: &Path,
    width: u32,
    height: u32,
    pages_to_turn: Option<usize>,
) -> Result<(), String> {
    let start_read = Instant::now();
    let bytes = fs::read(book_path)
        .map_err(|e| format!("Failed to read book file {}: {e}", book_path.display()))?;
    let _read_duration = start_read.elapsed();

    let file_name = book_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid file name: {}", book_path.display()))?;

    let engine = Engine::default();
    let start_open = Instant::now();
    let book = engine
        .open_bytes(Arc::<[u8]>::from(bytes), file_name)
        .map_err(|e| format!("Failed to open book {}: {e}", book_path.display()))?;
    let open_duration = start_open.elapsed();

    let start_reader = Instant::now();
    let mut reader = engine
        .create_reader(
            &book,
            ReaderConfig {
                viewport: LayoutViewport { width, height },
                style: ReaderStyle::default(),
                locator: None,
            },
        )
        .map_err(|e| format!("Failed to create reader for {}: {e}", book_path.display()))?;

    let spread = reader
        .current_spread()
        .map_err(|e| format!("Failed to get current spread: {e}"))?;
    let first_page_duration = start_reader.elapsed();

    let snapshot = reader.snapshot();
    let primary = &spread.primary;
    let secondary_dim = spread.secondary.as_ref().map_or_else(
        || "none".to_string(),
        |s| format!("{}x{}", s.width(), s.height()),
    );

    let command_count =
        primary.command_count() + spread.secondary.as_ref().map_or(0, |s| s.command_count());
    let text_region_count = primary.text_region_count()
        + spread
            .secondary
            .as_ref()
            .map_or(0, |s| s.text_region_count());

    println!("=== Book Pagination ===");
    println!("File:                     {}", book_path.display());
    println!("Viewport:                 {width}x{height}");
    println!("Open Duration:            {open_duration:.2?}");
    println!("First Page Duration:      {first_page_duration:.2?}");
    println!(
        "Location:                 section={}, segment={}/{}, page={}/{}",
        snapshot.location.section_index,
        snapshot.location.segment_index,
        snapshot.location.segment_count,
        snapshot.location.page_index,
        snapshot.location.page_count
    );
    println!(
        "Total Progression:        {:.2}%",
        snapshot.total_progression * 100.0
    );
    println!(
        "Primary Page Size:        {}x{}",
        primary.width(),
        primary.height()
    );
    println!("Secondary Page Size:      {secondary_dim}");
    println!("Display Commands:         {command_count}");
    println!("Text Regions:             {text_region_count}");

    let start_prefetch = Instant::now();
    reader
        .prefetch_adjacent()
        .map_err(|e| format!("Prefetch failed: {e}"))?;
    println!("Prefetch Queued In:       {:.2?}", start_prefetch.elapsed());

    if let Some(n) = pages_to_turn {
        println!("\nAdvancing through {n} pages:");
        for step in 1..=n {
            let turn_start = Instant::now();
            let outcome = reader
                .try_turn_page(PageDirection::Next)
                .map_err(|e| format!("Error turning page: {e}"))?;
            let turn_elapsed = turn_start.elapsed();

            match outcome {
                NavigationAttempt::Ready(result) => {
                    println!(
                        "  Step {step}: {:?} (section={}, page={}) in {turn_elapsed:.2?}",
                        result.outcome,
                        result.snapshot.location.section_index,
                        result.snapshot.location.page_index
                    );
                }
                NavigationAttempt::Pending => {
                    println!("  Step {step}: Pending background preparation in {turn_elapsed:.2?}");
                }
            }
        }
    }

    Ok(())
}
