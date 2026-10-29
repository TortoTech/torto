# Torto system-font provider dependency patch

Based on the crates.io source of `egui_system_font_provider 0.37.0`, licensed
under MIT OR Apache-2.0. The provider's Rust source and public API are unchanged.

The normalized manifest uses Fontique 0.12.0 instead of 0.11.1, matching Parley
0.12 and avoiding a second copy of system font discovery and fallback code.
The original upstream manifest and version-control metadata are retained.
The desktop still wraps the provider with its asynchronous, bounded lookup
adapter; font enumeration and file access must remain off the UI thread.

Remove this override when an upstream egui system-font provider release uses
the same Fontique version as Torto's text layout stack and font regressions pass.
