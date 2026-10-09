# Local Hayro 0.7.1 patch

Copied from the crates.io Hayro 0.7.1 release (Apache-2.0 OR MIT). Both original license files are retained. The interpreter and syntax crates remain unmodified upstream dependencies.

- `render_region` adds an integer pixel origin while retaining the page scale, transform, compositing, masks and clipping. Native conversion uses one padded union viewport per physical page, aligned to Vello CPU wide tiles (256 x 4 pixels) so changing the viewport does not change edge sampling.
- Image decoder resolution hints use transformed **vectors**, excluding translation. Otherwise changing a crop origin changes JPEG decoding resolution. Out-of-viewport images are skipped before decoding.
- `render_embedded_image` samples a proven isolated opaque image through the same renderer image pipeline. Torto uses the existing PDF decoder, including CMYK/calibrated colors, rather than interpreting JPEG color independently. Sampling preserves the ordinary crop dimensions.
- `RenderCache::with_outline_budget` limits retained outline accounting (including conservative Vec/hash-table overhead). Oversized outlines are not retained. `begin_page` releases interpreted fonts, images and color objects; outline reuse remains confined to one conversion worker and document. Existing default caches are unchanged.

Synthetic format tests compare viewports across page rotation, nonzero page origins, clipping and alpha masks/transparency, and reject unsafe isolated-image candidates. Subpixel floating-point rounding in the CPU renderer can differ by a few color levels under compositing; geometry and output dimensions must match. Format tests exercise zero/small outline budgets across page resets and compare their actual rendered pixels against fresh unbounded caches.
