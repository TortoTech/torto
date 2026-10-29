# Torto epaint patch

Based on the crates.io source of epaint 0.37.0, licensed under MIT OR Apache-2.0.

Vello CPU is upgraded from upstream 0.2 to 0.3.0, matching the PDF and generated
cover backends. In `src/text/font_face.rs` both outline and color-glyph contexts
flush before rasterization, and color-glyph filling handles the new fallible API.
This shares one CPU renderer version without changing the desktop GPU backend.

The former font-byte sharing and glyph-coverage patches are retired: 0.37 stores
shared `FontData::Blob` bytes and resolves glyph coverage through its new face
store. The desktop disables `default_fonts` and uses system fonts, including
native colored Emoji, through an asynchronous adapter. Tests alone enable default
Latin fonts for independent framework fixtures; release builds embed none of them.
