//! Headless allocation probes. Run each case in a fresh process:
//! cargo run -p rebook-desktop --example memory_probe -- svg
//! cargo run -p rebook-desktop --example memory_probe -- layout
//! Add --features memory-profiling to record live Rust heap bytes too.
//! Compare gpu/gpu-memory or gpu-vulkan/gpu-vulkan-memory in separate processes.
//! Add `-render` to execute Vello's pipeline, or `-buffers` to allocate matching
//! buffers without Vello. `-novalidation` emulates production instance flags;
//! `-serial` compiles Vello's shaders on a single thread for comparison.
//! `-reuse` keeps one Vello renderer across the three cycles.

use std::sync::Arc;

#[cfg(all(target_os = "windows", feature = "memory-profiling"))]
#[global_allocator]
static ALLOCATOR: rebook_windows_window_background::CountingAllocator =
    rebook_windows_window_background::CountingAllocator;

fn sample(stage: &str) {
    #[cfg(all(target_os = "windows", feature = "memory-profiling"))]
    {
        let (live, peak) = rebook_windows_window_background::rust_allocation_bytes();
        println!(
            "Rust heap: live={:.2} MiB peak={:.2} MiB",
            live as f64 / 1_048_576.0,
            peak as f64 / 1_048_576.0
        );
    }
    #[cfg(target_os = "windows")]
    if let Some((working, private)) = rebook_windows_window_background::process_memory() {
        println!(
            "{stage}: working={:.2} MiB private={:.2} MiB",
            working as f64 / 1_048_576.0,
            private as f64 / 1_048_576.0
        );
    }
    #[cfg(all(target_os = "windows", feature = "memory-profiling"))]
    if let Some(heap) = rebook_windows_window_background::process_heap_stats() {
        println!(
            "Win32 default heap: busy={:.2} MiB free={:.2} MiB region_committed={:.2} MiB entries={}",
            heap.busy_bytes as f64 / 1_048_576.0,
            heap.free_bytes as f64 / 1_048_576.0,
            heap.region_committed_bytes as f64 / 1_048_576.0,
            heap.entries
        );
    }
    #[cfg(not(target_os = "windows"))]
    println!("{stage}: process counters unavailable");
}

fn main() {
    sample("baseline");
    match std::env::args().nth(1).as_deref() {
        Some(mode) if mode.starts_with("gpu") => {
            pollster::block_on(async {
                let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                    backends: if mode.starts_with("gpu-vulkan") {
                        wgpu::Backends::VULKAN
                    } else {
                        wgpu::Backends::DX12
                    },
                    flags: if mode.contains("novalidation") {
                        wgpu::InstanceFlags::empty()
                    } else {
                        wgpu::InstanceFlags::default()
                    },
                    ..wgpu::InstanceDescriptor::new_without_display_handle()
                });
                let adapter = instance
                    .request_adapter(&wgpu::RequestAdapterOptions::default())
                    .await
                    .unwrap();
                println!("adapter={:?}", adapter.get_info());
                sample("adapter discovered");
                let (device, queue) = adapter
                    .request_device(&wgpu::DeviceDescriptor {
                        memory_hints: if mode.contains("-memory") {
                            wgpu::MemoryHints::MemoryUsage
                        } else {
                            wgpu::MemoryHints::Performance
                        },
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                sample("GPU device created");
                gpu_sample(&device);
                let gui = egui_wgpu::Renderer::new(
                    &device,
                    wgpu::TextureFormat::Bgra8UnormSrgb,
                    egui_wgpu::RendererOptions::default(),
                );
                sample("egui renderer created");
                gpu_sample(&device);
                if mode.contains("-buffers") {
                    // Match Vello's largest fixed buffers, without shaders,
                    // scenes, books or a window. Isolates native GPU allocation.
                    for cycle in 0..3 {
                        println!("buffer cycle={cycle}");
                        let buffers: Vec<_> = [48, 48, 32, 16, 16, 4, 1]
                            .into_iter()
                            .map(|mib| {
                                device.create_buffer(&wgpu::BufferDescriptor {
                                    label: Some("memory-probe-buffer"),
                                    size: mib * 1024 * 1024,
                                    usage: wgpu::BufferUsages::STORAGE,
                                    mapped_at_creation: false,
                                })
                            })
                            .collect();
                        sample("GPU buffers allocated");
                        gpu_sample(&device);
                        drop(buffers);
                        device
                            .poll(wgpu::PollType::Wait {
                                submission_index: None,
                                timeout: Some(std::time::Duration::from_secs(10)),
                            })
                            .unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        sample("GPU buffers freed and idle");
                        gpu_sample(&device);
                    }
                }
                let mut retained_renderer = None;
                for cycle in 0..3 {
                    println!("GPU cycle={cycle}");
                    if retained_renderer.is_none() {
                        retained_renderer = Some(
                            vello::Renderer::new(
                                &device,
                                vello::RendererOptions {
                                    antialiasing_support: vello::AaSupport::area_only(),
                                    num_init_threads: if mode.contains("serial") {
                                        std::num::NonZeroUsize::new(1)
                                    } else {
                                        None
                                    },
                                    ..Default::default()
                                },
                            )
                            .unwrap(),
                        );
                    }
                    let renderer = retained_renderer.as_mut().unwrap();
                    sample("Vello renderer ready");
                    gpu_sample(&device);
                    if mode.contains("-render") {
                        let texture = device.create_texture(&wgpu::TextureDescriptor {
                            label: Some("memory-probe-page"),
                            size: wgpu::Extent3d {
                                width: 2880,
                                height: 1704,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            usage: wgpu::TextureUsages::STORAGE_BINDING,
                            view_formats: &[],
                        });
                        let mut scene = vello::Scene::new();
                        scene.fill(
                            peniko::Fill::NonZero,
                            kurbo::Affine::IDENTITY,
                            peniko::Color::WHITE,
                            None,
                            &kurbo::Rect::new(0.0, 0.0, 1000.0, 800.0),
                        );
                        renderer
                            .render_to_texture(
                                &device,
                                &queue,
                                &scene,
                                &texture.create_view(&Default::default()),
                                &vello::RenderParams {
                                    base_color: peniko::Color::BLACK,
                                    width: 2880,
                                    height: 1704,
                                    antialiasing_method: vello::AaConfig::Area,
                                },
                            )
                            .unwrap();
                        device
                            .poll(wgpu::PollType::Wait {
                                submission_index: None,
                                timeout: Some(std::time::Duration::from_secs(10)),
                            })
                            .unwrap();
                        sample("Vello page rendered");
                        gpu_sample(&device);
                        drop((texture, scene));
                    }
                    if !mode.contains("-reuse") {
                        drop(retained_renderer.take());
                    }
                    device
                        .poll(wgpu::PollType::Wait {
                            submission_index: None,
                            timeout: Some(std::time::Duration::from_secs(10)),
                        })
                        .unwrap();
                    sample(if mode.contains("-reuse") {
                        "Vello renderer retained and device polled"
                    } else {
                        "Vello renderer dropped and device polled"
                    });
                    gpu_sample(&device);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    sample("Vello resources after driver idle");
                    gpu_sample(&device);
                }
                drop(retained_renderer);
                device
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(std::time::Duration::from_secs(10)),
                    })
                    .unwrap();
                sample("all Vello renderers dropped");
                gpu_sample(&device);
                drop((gui, queue, device, adapter, instance));
                sample("GPU device dropped");
                std::thread::sleep(std::time::Duration::from_millis(500));
                sample("GPU device after driver idle");
            });
        }
        Some("svg") => {
            let mut options = resvg::usvg::Options::default();
            options.fontdb_mut().load_system_fonts();
            println!("SVG faces={}", options.fontdb.faces().count());
            sample("system SVG fonts loaded");
            drop(options);
            sample("system SVG fonts dropped");
        }
        Some("layout") => {
            let fonts = [
                peniko::Blob::new(Arc::new(include_bytes!(
                    "../../../assets/fonts/Literata-opsz-wght.ttf"
                ) as &'static [u8])),
                peniko::Blob::new(Arc::new(include_bytes!(
                    "../../../assets/fonts/Literata-Italic-opsz-wght.ttf"
                ) as &'static [u8])),
                peniko::Blob::new(Arc::new(rebook_formats::cjk_fallback_font_bytes())),
            ];
            let mut first = rebook_layout::LayoutEngine::with_fonts(fonts.iter().cloned());
            sample("foreground engine");
            let second = rebook_layout::LayoutEngine::with_fonts(fonts.iter().cloned());
            sample("foreground and prefetch engines");
            let families = first.available_reader_font_families();
            println!("reader families={}", families.all.len());
            sample("font choices inspected");
            drop((first, second));
            sample("engines dropped");
        }
        _ => {
            eprintln!(
                "usage: memory_probe svg|layout|gpu[-vulkan][-render|-buffers][-memory][-novalidation][-serial][-reuse]"
            )
        }
    }
}

fn gpu_sample(device: &wgpu::Device) {
    if let Some(report) = device.generate_allocator_report() {
        println!(
            "GPU allocator: allocated={:.2} MiB reserved={:.2} MiB",
            report.total_allocated_bytes as f64 / 1_048_576.0,
            report.total_reserved_bytes as f64 / 1_048_576.0
        );
        let mut allocations = report.allocations;
        allocations.sort_by_key(|allocation| std::cmp::Reverse(allocation.size));
        for allocation in allocations.iter().take(5) {
            println!(
                "  {}: {:.2} MiB",
                allocation.name,
                allocation.size as f64 / 1_048_576.0
            );
        }
    }
}
