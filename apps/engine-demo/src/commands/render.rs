use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rebook_engine::{Engine, ReaderConfig};
use rebook_layout::{LayoutViewport, ReaderStyle};

use crate::metrics::{DemoMetricsReport, MetricsFormat, PercentileSummary, PipelineMetrics};
use crate::render::OffscreenTarget;
use crate::render::metrics::RenderMetrics;
use crate::render::scene_cache::ResourceProfile;

fn print_render_metrics(output: &Path, metrics: &RenderMetrics) {
    println!("=== Render Output ===");
    println!("Output File:              {}", output.display());
    println!(
        "Dimensions:               {}x{}",
        metrics.width, metrics.height
    );
    println!("Referenced Images:        {}", metrics.image_count);
    println!("Scene Build CPU Time:     {:.2?}", metrics.scene_build);
    println!("Vello Submit Time:        {:.2?}", metrics.gpu_submit);
    println!("GPU Readback Time:        {:.2?}", metrics.readback);
    println!("PNG Encode Time:          {:.2?}", metrics.png_encode);
    println!(
        "Scene Cache:              hits={}, misses={}, builds={}, evictions={}",
        metrics.cache.hits, metrics.cache.misses, metrics.cache.builds, metrics.cache.evictions
    );
}

fn export_metrics(
    report: &DemoMetricsReport,
    metrics_format: Option<MetricsFormat>,
    metrics_file: Option<&Path>,
) -> Result<(), String> {
    let formatted = match metrics_format {
        Some(MetricsFormat::Json) => serde_json::to_string_pretty(report)
            .map_err(|e| format!("Failed to serialize metrics to json: {e}"))?,
        Some(MetricsFormat::Text) => format!("{report:#?}"),
        None => String::new(),
    };

    if let Some(format) = metrics_format {
        println!("\n=== Metrics ({format:?}) ===");
        println!("{formatted}");
    }

    if let Some(path) = metrics_file {
        let content = if formatted.is_empty() {
            serde_json::to_string_pretty(report)
                .map_err(|e| format!("Failed to serialize metrics: {e}"))?
        } else {
            formatted
        };
        fs::write(path, content)
            .map_err(|e| format!("Failed to write metrics to {}: {e}", path.display()))?;
    }
    Ok(())
}

pub fn run(
    book_path: &Path,
    output: &Path,
    width: u32,
    height: u32,
    metrics_format: Option<MetricsFormat>,
    metrics_file: Option<&Path>,
    profile: ResourceProfile,
) -> Result<(), String> {
    let start_read = Instant::now();
    let bytes = fs::read(book_path)
        .map_err(|e| format!("Failed to read book file {}: {e}", book_path.display()))?;
    let file_read_dur = start_read.elapsed();

    let file_name = book_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid file name: {}", book_path.display()))?;

    let engine = Engine::default();

    let start_open = Instant::now();
    let book = engine
        .open_bytes(Arc::<[u8]>::from(bytes), file_name)
        .map_err(|e| format!("Failed to open book {}: {e}", book_path.display()))?;
    let publication_open_dur = start_open.elapsed();

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
    let reader_and_first_page_dur = start_reader.elapsed();

    let mut target = pollster::block_on(OffscreenTarget::with_profile(profile))?;
    let (png_bytes, render_metrics) = target.render_spread_to_png(&spread, width, height)?;

    fs::write(output, &png_bytes)
        .map_err(|e| format!("Failed to write output image {}: {e}", output.display()))?;

    print_render_metrics(output, &render_metrics);

    let pipeline = PipelineMetrics::from_durations(
        file_read_dur,
        publication_open_dur,
        reader_and_first_page_dur,
        render_metrics.scene_build,
        render_metrics.gpu_submit,
        Some(render_metrics.readback),
    );

    let report = DemoMetricsReport {
        viewport: (width, height),
        adapter_backend: "wgpu-vello-offscreen".to_string(),
        spread_mode: "Single".to_string(),
        transition_kind: "None".to_string(),
        resource_profile: format!("{profile:?}"),
        pipeline: Some(pipeline),
        total_frames: 1,
        missed_60hz_count: 0,
        missed_120hz_count: 0,
        cpu_frame_percentiles: PercentileSummary {
            p50_ms: render_metrics.scene_build.as_secs_f64() * 1000.0,
            p95_ms: render_metrics.scene_build.as_secs_f64() * 1000.0,
            p99_ms: render_metrics.scene_build.as_secs_f64() * 1000.0,
        },
        gpu_frame_percentiles: Some(PercentileSummary {
            p50_ms: render_metrics.gpu_submit.as_secs_f64() * 1000.0,
            p95_ms: render_metrics.gpu_submit.as_secs_f64() * 1000.0,
            p99_ms: render_metrics.gpu_submit.as_secs_f64() * 1000.0,
        }),
        cache: render_metrics.cache,
    };

    export_metrics(&report, metrics_format, metrics_file)?;

    Ok(())
}
