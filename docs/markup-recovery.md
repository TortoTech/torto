# Publication content recovery

`crates/formats/src/markup.rs` owns shared, bounded HTML/XML recovery. EPUB
content, direct converted sources, MOBI/AZW/AZW3 and CHM use this layer. FB2/FBZ
use its conservative XML mode before conversion. ZIP integrity, package/spine
metadata, NCX navigation, PDF and CBZ structure keep their format-specific checks.

The normal path returns well-formed content byte-for-byte. Recovery first tries
token-level repairs: HTML void tags, duplicate HTML attributes (first wins), and
missing closing tags whose enclosing boundary is explicit. Optional paragraph,
list and table end tags and ambiguous HTML nesting fall back to the existing
HTML5 parser. Serialization preserves namespace-qualified attributes, SVG,
MathML, styles and resource/anchor references, and uses XML-safe text and void
elements. It does not insert synthetic text or accept an empty recovered body.

FB2 never goes through HTML5. Only missing paragraph/inline ends inside the body
can be closed against an explicit ancestor. Ambiguous title/section/metadata
boundaries, conflicting attributes and truncated XML remain errors. Common
non-XML text entities and bare ampersands can be normalized without touching
CDATA or binary image payloads.

Byte, token/node and depth limits apply before recovery and to its output.
External DTDs are never fetched; internal subsets and entity declarations are
rejected. Recovery diagnostics go to stderr, not additional reader dialogs.
Original book resources are unchanged. The reader's existing section cache
reuses the parsed results; no repaired book or persistent recovery cache is
written. Deterministic recovery keeps repeated parses consistent, but edits to
an already malformed source can naturally change its recovered structure.

Tests cover content, anchors, links, attributes, foreign namespaces, repeated
recovery, limits and format conversion. Local library audits should compare
reading order, TOC, block counts and anchor counts before and after changes;
these checks complement, rather than replace, text/resource regression tests.
