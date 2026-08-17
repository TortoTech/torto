# Local compatibility patch

Torto uses Vello 0.10 and egui-wgpu in the same render pass. Vello 0.10 currently
targets wgpu 29, while egui-wgpu 0.36.1 targets wgpu 30. This vendored crate keeps
egui-wgpu 0.36.1 on wgpu 29 until Vello publishes a compatible release so both
renderers share the same device, queue, textures, and command buffers.

The small source changes remove wgpu 30-only adapter metadata/options and use
wgpu 29's vertex-buffer layout shape.
