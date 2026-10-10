# Retired compatibility patch

As of 2026-10-09 this directory is excluded and no longer referenced by `[patch.crates-io]`. The desktop uses official egui-wgpu 0.36.2 and Vello 0.11.0 on wgpu 30.0.1. The old wgpu 29 adapter below is retained only as historical source.

# Local compatibility patch

Torto uses Vello 0.10 and egui-wgpu in the same render pass. Vello 0.10 currently
targets wgpu 29, while egui-wgpu 0.36.1 targets wgpu 30. This vendored crate keeps
egui-wgpu 0.36.1 on wgpu 29 until Vello publishes a compatible release so both
renderers share the same device, queue, textures, and command buffers.

The small source changes remove wgpu 30-only adapter metadata/options and use
wgpu 29's vertex-buffer layout shape.
