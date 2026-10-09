//! Headless diagnostic for the same local conversion used by the desktop.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().ok_or("PDF path is required")?);
    let output = args
        .next()
        .ok_or("Output directory or --assess is required")?;
    if output == "--assess" {
        let started = std::time::Instant::now();
        let assessment =
            rebook_formats::pdf_reflow::assess(&input, "diagnostic", &AtomicBool::new(false))?;
        println!(
            "{}",
            serde_json::json!({"assessment":assessment,"elapsed_seconds":started.elapsed().as_secs_f64()})
        );
        return Ok(());
    }
    let output = PathBuf::from(output);
    if output.exists() {
        return Err("Output directory must be new".into());
    }
    let (book, origin) = {
        let opened = rebook_formats::open_file_for_reading(&input, None)?;
        (
            opened.book().clone(),
            opened.source().table_of_contents_origin(),
        )
    };
    let started = std::time::Instant::now();
    let result = rebook_formats::pdf_reflow::convert(
        &input,
        &book,
        origin,
        &output,
        &AtomicBool::new(false),
        |p, n, phase| {
            if p == n || p % 50 == 0 {
                eprintln!("{phase} {p}/{n}");
            }
        },
    );
    let manifest = match result {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&output);
            return Err(error.into());
        }
    };
    let source = rebook_formats::pdf_reflow::ReflowSource::open(&output, book.id.as_str())?;
    use rebook_publication::BookSource;
    for index in 0..source.book().sections.len() {
        source.parse_section(index)?;
        source.provenance(index)?;
    }
    println!(
        "{}",
        serde_json::json!({"elapsed_seconds":started.elapsed().as_secs_f64(),"timings":manifest.stats.timings,"stats":manifest.stats,"book":manifest.book.metadata.title,"sections":manifest.book.sections.len()})
    );
    Ok(())
}
