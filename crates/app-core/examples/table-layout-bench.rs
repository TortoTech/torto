//! Local, offline whole-section benchmark. Pass publication paths as arguments.
//! Run a built binary without concurrent compilation; no AI or cloud requests.
use rebook_layout::{LayoutEngine, LayoutViewport, ReaderStyle, ReaderTypesetting};
use rebook_publication::Block;
use std::{path::PathBuf, time::Instant};

fn counts(blocks: &[Block]) -> (usize, usize, usize) {
    blocks
        .iter()
        .fold((0, 0, 0), |(mut tables, mut cells, mut spans), block| {
            let found = match block {
                Block::Table(table) => (
                    1,
                    table.rows.iter().map(|r| r.cells.len()).sum(),
                    table
                        .rows
                        .iter()
                        .flat_map(|r| &r.cells)
                        .filter(|c| c.row_span > 1 || c.column_span > 1)
                        .count(),
                ),
                Block::Figure(figure) => counts(&figure.body),
                Block::Note(note) => counts(&note.blocks),
                _ => (0, 0, 0),
            };
            tables += found.0;
            cells += found.1;
            spans += found.2;
            (tables, cells, spans)
        })
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("Pass one or more local publication paths".into());
    }
    for path in paths {
        let opened = rebook_formats::open_file_for_reading(path, None)?;
        let source = opened.source();
        let mut sections = Vec::new();
        for index in 0..source.book().sections.len() {
            let section = source.parse_section(index)?;
            let (tables, cells, spans) = counts(&section.blocks);
            if tables > 0 {
                sections.push((index, tables, cells, spans, section));
            }
        }
        let mut selected = Vec::new();
        for key in [1, 2] {
            if let Some(sample) = sections
                .iter()
                .max_by_key(|s| if key == 1 { s.1 } else { s.2 })
            {
                if !selected.contains(&sample.0) {
                    selected.push(sample.0);
                }
            }
        }
        for (index, tables, cells, spans, section) in
            sections.into_iter().filter(|s| selected.contains(&s.0))
        {
            for width in [800, 1200] {
                let mut style = ReaderStyle {
                    typesetting: ReaderTypesetting::unified(),
                    ..Default::default()
                };
                style.typography.font_size = 20.;
                let mut engine = LayoutEngine::with_fonts(
                    rebook_app_core::fonts::reader_fonts().iter().cloned(),
                );
                engine.set_deferred_images(true);
                let mut ms = Vec::new();
                let mut pages = 0;
                let mut table_height = 0.;
                for _ in 0..4 {
                    let start = Instant::now();
                    let layout = engine.layout_section(
                        source.as_ref(),
                        &section,
                        LayoutViewport::new(width, 1200)?,
                        &style,
                    )?;
                    ms.push(start.elapsed().as_secs_f64() * 1000.);
                    pages = layout.pages.len();
                    table_height = layout
                        .pages
                        .iter()
                        .flat_map(|p| &p.items)
                        .filter_map(|item| match item {
                            rebook_layout::PageItem::Table(table) => Some(table.height),
                            _ => None,
                        })
                        .sum::<f32>();
                }
                let stats = engine.table_width_stats();
                println!(
                    "{}",
                    serde_json::json!({"book":source.book().metadata.title,"section":index,
                    "tables":tables,"cells":cells,"spans":spans,"width":width,"cold_ms":ms[0],
                    "warm_ms":ms[1..],"pages":pages,"table_height":table_height,
                    "searches":stats.searches,"improvements":stats.improvements,"cache_hits":stats.cache_hits,
                    "measurements":stats.measurements,"profile_limits":stats.profile_limits})
                );
            }
        }
    }
    Ok(())
}
