# Local Hayro 0.8.0 patch

Copied from the crates.io Hayro 0.8.0 release (Apache-2.0 OR MIT). Both original license files are retained. Interpreter and syntax 0.8.0 remain unmodified upstream dependencies.

- The renderer uses upstream `render_into` and its translation-independent image-resolution calculation. The old renderer module, custom `render_region` and decode-vector workaround are removed. Torto constructs pixel-aligned viewports in `crates/formats/src/pdf.rs`; conversion retains the Vello CPU 256 x 4 tile grid and padded union viewport.
- `render_embedded_image` samples a proven isolated opaque decoded image through the same upstream PDF image sampler, with the ordinary crop dimensions. It retains authoritative calibrated/CMYK color decoding and uses explicit single-threaded rendering.
- `RenderCache::with_outline_budget` limits retained outlines, including conservative Vec/hash-table overhead. Oversized outlines are not cached. `begin_page` releases interpreted fonts, images and color objects; outline reuse is confined to one conversion worker/document. Default caches remain unchanged.

Vello CPU 0.3.0 is already the upstream dependency. Native generation V13 isolates changed interpretation and derived pixels from prior caches.

Existing format regressions compare rotation, nonzero page origins, clipping, masks/transparency, isolated-image eligibility, direct-export sampling, and zero/small outline budgets across page resets.
