# Reflowable image dimensions

HTML dimension hints are element-specific. Width/height attributes on ordinary
paragraphs, divs or blockquotes do not establish image containing blocks. Supported
image and table/cell hints follow HTML dimension parsing, including the nonzero
rule for table/cell widths. Attributes remain available to CSS selectors. Author
CSS overrides hints, including auto and inherited values; invalid negative,
unitless nonzero or malformed dimension declarations do not replace earlier
valid declarations.

Containing width, definite containing height and overflow clipping are retained
separately while wrappers are flattened. Percentage image widths resolve against
the containing block, including a real zero. Percentage heights require a definite
containing height; ordinary content-sized parents leave them automatic. Image
minimum/maximum constraints and automatic axes use intrinsic dimensions and ratio.
Final fitting to a reader column/page is a separate display policy.

An automatic image can overflow a zero/narrow containing block when overflow is
visible. Hidden/clip/scroll/auto overflow retains a separate clip rectangle, rather
than resizing the image's pixels. Renderers and image hit bounds use the visible
rectangle; previews retain the original resource. Zero-sized or fully clipped
images produce no painted image. Source anchors are unchanged. Source IR revision
15 rebuilds previously cached container-width hints.

References: [HTML rendering](https://html.spec.whatwg.org/multipage/rendering.html),
[CSS sizing](https://www.w3.org/TR/CSS22/visudet.html), and
[CSS overflow](https://www.w3.org/TR/CSS22/visufx.html#overflow).

Regressions cover legacy p/div attributes, CSS priority/invalid declarations,
percentage heights, zero containing widths, percentage-sized children, minimum and
maximum image sizes, and non-rescaling overflow clips. The local algorithm-book
regression is documented in [AI layout](ai-semantic-layout.md#code-images-and-authored-code-containers).
