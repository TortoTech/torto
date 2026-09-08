use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rebook_engine::Engine;

pub fn run(book_path: &Path) -> Result<(), String> {
    let start_read = Instant::now();
    let bytes = fs::read(book_path)
        .map_err(|e| format!("Failed to read book file {}: {e}", book_path.display()))?;
    let read_duration = start_read.elapsed();

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

    let meta = &book.book().metadata;
    let cover_presence = if book.cover_bytes().is_some() {
        "yes"
    } else {
        "no"
    };

    println!("=== Book Inspection ===");
    println!("File:               {}", book_path.display());
    println!("Format:             {}", book.format());
    println!("Publication ID:     {}", book.book().id);
    println!("Title:              {}", meta.title);
    println!("Authors:            {}", meta.authors.join(", "));
    println!("Languages:          {}", meta.languages.join(", "));
    println!("Layout:             {:?}", meta.layout);
    println!("Sections:           {}", book.book().sections.len());
    println!(
        "TOC Entries:        {}",
        book.book().table_of_contents.len()
    );
    println!("Cover:              {cover_presence}");
    println!("File Read Time:     {read_duration:.2?}");
    println!("Open Duration:      {open_duration:.2?}");

    Ok(())
}
