use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Duration;

use crate::render::scene_cache::CacheMetrics;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetricsFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineMetrics {
    pub file_read_ms: f64,
    pub publication_open_ms: f64,
    pub reader_create_and_first_page_ms: f64,
    pub scene_build_ms: f64,
    pub gpu_submit_ms: f64,
    pub gpu_readback_ms: Option<f64>,
}

impl PipelineMetrics {
    pub fn from_durations(
        file_read: Duration,
        publication_open: Duration,
        reader_create_and_first_page: Duration,
        scene_build: Duration,
        gpu_submit: Duration,
        gpu_readback: Option<Duration>,
    ) -> Self {
        Self {
            file_read_ms: file_read.as_secs_f64() * 1000.0,
            publication_open_ms: publication_open.as_secs_f64() * 1000.0,
            reader_create_and_first_page_ms: reader_create_and_first_page.as_secs_f64() * 1000.0,
            scene_build_ms: scene_build.as_secs_f64() * 1000.0,
            gpu_submit_ms: gpu_submit.as_secs_f64() * 1000.0,
            gpu_readback_ms: gpu_readback.map(|d| d.as_secs_f64() * 1000.0),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct FrameStats {
    pub interval_ms: f64,
    pub cpu_duration_ms: f64,
    pub gpu_duration_ms: Option<f64>,
    pub scene_builds: usize,
    pub target_recreations: usize,
    pub missed_60hz: bool,
    pub missed_120hz: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PercentileSummary {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportContext {
    pub viewport: (u32, u32),
    pub adapter_backend: String,
    pub spread_mode: String,
    pub transition_kind: String,
    pub resource_profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoMetricsReport {
    pub viewport: (u32, u32),
    pub adapter_backend: String,
    pub spread_mode: String,
    pub transition_kind: String,
    pub resource_profile: String,
    pub pipeline: Option<PipelineMetrics>,
    pub total_frames: usize,
    pub missed_60hz_count: usize,
    pub missed_120hz_count: usize,
    pub cpu_frame_percentiles: PercentileSummary,
    pub gpu_frame_percentiles: Option<PercentileSummary>,
    pub cache: CacheMetrics,
}

pub struct RollingWindowMetrics {
    window_size: usize,
    cpu_samples: VecDeque<Duration>,
    gpu_samples: VecDeque<Duration>,
    missed_60hz_count: usize,
    missed_120hz_count: usize,
    total_frames: usize,
}

impl RollingWindowMetrics {
    pub fn new(window_size: usize) -> Self {
        Self {
            window_size: window_size.max(10),
            cpu_samples: VecDeque::with_capacity(window_size),
            gpu_samples: VecDeque::with_capacity(window_size),
            missed_60hz_count: 0,
            missed_120hz_count: 0,
            total_frames: 0,
        }
    }

    pub fn record_frame(&mut self, stats: &FrameStats) {
        self.total_frames += 1;
        if stats.missed_60hz {
            self.missed_60hz_count += 1;
        }
        if stats.missed_120hz {
            self.missed_120hz_count += 1;
        }

        if self.cpu_samples.len() >= self.window_size {
            self.cpu_samples.pop_front();
        }
        self.cpu_samples
            .push_back(Duration::from_secs_f64(stats.cpu_duration_ms / 1000.0));

        if let Some(gpu_ms) = stats.gpu_duration_ms {
            if self.gpu_samples.len() >= self.window_size {
                self.gpu_samples.pop_front();
            }
            self.gpu_samples
                .push_back(Duration::from_secs_f64(gpu_ms / 1000.0));
        }
    }

    pub fn cpu_percentiles(&self) -> (Duration, Duration, Duration) {
        calculate_percentiles(&self.cpu_samples)
    }

    pub fn gpu_percentiles(&self) -> Option<(Duration, Duration, Duration)> {
        if self.gpu_samples.is_empty() {
            None
        } else {
            Some(calculate_percentiles(&self.gpu_samples))
        }
    }

    pub fn total_frames(&self) -> usize {
        self.total_frames
    }

    pub fn missed_60hz_count(&self) -> usize {
        self.missed_60hz_count
    }

    pub fn missed_120hz_count(&self) -> usize {
        self.missed_120hz_count
    }

    pub fn build_report(
        &self,
        ctx: ReportContext,
        pipeline: Option<PipelineMetrics>,
        cache: CacheMetrics,
    ) -> DemoMetricsReport {
        let (cpu_p50, cpu_p95, cpu_p99) = self.cpu_percentiles();
        let cpu_frame_percentiles = PercentileSummary {
            p50_ms: cpu_p50.as_secs_f64() * 1000.0,
            p95_ms: cpu_p95.as_secs_f64() * 1000.0,
            p99_ms: cpu_p99.as_secs_f64() * 1000.0,
        };

        let gpu_frame_percentiles =
            self.gpu_percentiles()
                .map(|(g50, g95, g99)| PercentileSummary {
                    p50_ms: g50.as_secs_f64() * 1000.0,
                    p95_ms: g95.as_secs_f64() * 1000.0,
                    p99_ms: g99.as_secs_f64() * 1000.0,
                });

        DemoMetricsReport {
            viewport: ctx.viewport,
            adapter_backend: ctx.adapter_backend,
            spread_mode: ctx.spread_mode,
            transition_kind: ctx.transition_kind,
            resource_profile: ctx.resource_profile,
            pipeline,
            total_frames: self.total_frames,
            missed_60hz_count: self.missed_60hz_count,
            missed_120hz_count: self.missed_120hz_count,
            cpu_frame_percentiles,
            gpu_frame_percentiles,
            cache,
        }
    }
}

fn calculate_percentiles(samples: &VecDeque<Duration>) -> (Duration, Duration, Duration) {
    if samples.is_empty() {
        return (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    }
    let mut sorted: Vec<Duration> = samples.iter().copied().collect();
    sorted.sort();

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let p50_idx = ((sorted.len() as f64 * 0.50).floor() as usize).min(sorted.len() - 1);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let p95_idx = ((sorted.len() as f64 * 0.95).floor() as usize).min(sorted.len() - 1);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let p99_idx = ((sorted.len() as f64 * 0.99).floor() as usize).min(sorted.len() - 1);

    (sorted[p50_idx], sorted[p95_idx], sorted[p99_idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_calculated_correctly() {
        let mut window = RollingWindowMetrics::new(100);
        for i in 1..=100 {
            window.record_frame(&FrameStats {
                interval_ms: 16.6,
                cpu_duration_ms: f64::from(i),
                gpu_duration_ms: None,
                scene_builds: 0,
                target_recreations: 0,
                missed_60hz: false,
                missed_120hz: false,
            });
        }
        let (p50, p95, p99) = window.cpu_percentiles();
        assert_eq!(p50, Duration::from_millis(51));
        assert_eq!(p95, Duration::from_millis(96));
        assert_eq!(p99, Duration::from_millis(100));
    }
}
