# Torto epaint patch

Based on the crates.io source of epaint 0.36.1, licensed under MIT OR Apache-2.0.

In `src/text/fonts.rs`, parsed font faces retain the existing `Arc<FontData>` as their byte blob. This avoids cloning an owned font byte buffer while keeping the bytes alive for the face's lifetime. Public font APIs, font selection, shaping and rasterization are unchanged. The regression test checks both byte sharing and ownership after the definitions' reference is dropped.

Keep this patch when updating epaint until the upstream version shares runtime-loaded font bytes internally.

In `src/text/font.rs`, `has_glyph` checks the resolved face's actual character
mapping. Comparing the face ID with the replacement face ID incorrectly reports
supported characters as missing whenever their font also contains the replacement
glyph. Accurate coverage checks allow Torto to load interface fallback fonts only
when displayed text needs them.
