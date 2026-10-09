use super::*;
use hayro::hayro_syntax::object::{Array, Dict, Name, Object, Stream, String as PdfString};

pub(super) type StructureTags = HashMap<(usize, i32), Option<String>>;

pub(super) fn structure_tags(pdf: &Pdf) -> StructureTags {
    let mut tags = HashMap::new();
    let Some(catalog) = pdf.xref().get::<Dict<'_>>(pdf.xref().root_id()) else {
        return tags;
    };
    let Some(root) = catalog.get::<Dict<'_>>(b"StructTreeRoot") else {
        return tags;
    };
    let role_map = root.get::<Dict<'_>>(b"RoleMap");
    let pages: HashMap<_, _> = pdf
        .pages()
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.raw().obj_id().map(|id| (id, i)))
        .collect();
    fn visit<'a>(
        object: Object<'a>,
        page: Option<usize>,
        role: Option<String>,
        pages: &HashMap<hayro::hayro_syntax::object::ObjectIdentifier, usize>,
        role_map: Option<&Dict<'a>>,
        tags: &mut StructureTags,
        seen: &mut std::collections::HashSet<(usize, usize)>,
        depth: usize,
    ) {
        if depth > 64 || seen.len() > 100_000 {
            return;
        }
        let mut bind = |mcid: i32, role: Option<String>| {
            if let Some(page) = page {
                tags.entry((page, mcid))
                    .and_modify(|old| {
                        if *old != role {
                            *old = None;
                        }
                    })
                    .or_insert(role);
            }
        };
        match object {
            Object::Number(n) => bind(n.as_i64() as i32, role),
            Object::Array(array) => {
                for child in array.iter::<Object<'a>>() {
                    visit(
                        child,
                        page,
                        role.clone(),
                        pages,
                        role_map,
                        tags,
                        seen,
                        depth + 1,
                    );
                }
            }
            Object::Dict(dict) => {
                if !seen.insert((dict.data().as_ptr() as usize, dict.data().len()))
                    || dict.get::<Object<'a>>(b"Stm").is_some()
                {
                    return;
                }
                let page = dict
                    .get::<Dict<'a>>(b"Pg")
                    .and_then(|p| p.obj_id())
                    .and_then(|id| pages.get(&id).copied())
                    .or(page);
                let role = dict
                    .get::<Name<'a>>(b"S")
                    .map(|name| {
                        role_map
                            .and_then(|map| map.get::<Name<'a>>(name.as_ref()))
                            .map_or_else(
                                || String::from_utf8_lossy(name.as_ref()).into_owned(),
                                |name| String::from_utf8_lossy(name.as_ref()).into_owned(),
                            )
                    })
                    .or(role);
                if let Some(mcid) = dict.get::<hayro::hayro_syntax::object::Number>(b"MCID")
                    && let Some(page) = page
                {
                    let key = (page, mcid.as_i64() as i32);
                    tags.entry(key)
                        .and_modify(|old| {
                            if *old != role {
                                *old = None;
                            }
                        })
                        .or_insert(role.clone());
                }
                if let Some(k) = dict.get::<Object<'a>>(b"K") {
                    visit(k, page, role, pages, role_map, tags, seen, depth + 1);
                }
            }
            _ => {}
        }
    }
    if let Some(k) = root.get::<Object<'_>>(b"K") {
        visit(
            k,
            None,
            None,
            &pages,
            role_map.as_ref(),
            &mut tags,
            &mut std::collections::HashSet::new(),
            0,
        );
    }
    tags
}

#[derive(Default)]
struct Extractor {
    page: NativePage,
    fonts: HashMap<u128, (bool, bool)>,
    tags: Vec<(String, Option<i32>)>,
    clips: Vec<Rect>,
    rectangular_clips: Vec<bool>,
    opaque_groups: Vec<bool>,
    soft_mask: bool,
    normal_blend: bool,
}

pub(super) fn page(pdf: &Pdf, index: usize, tags: &StructureTags) -> NativePage {
    let page = &pdf.pages()[index];
    let (width, height) = page.render_dimensions();
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        &cache,
        page.xref(),
        interpreter_settings(),
    );
    let mut extractor = Extractor {
        normal_blend: true,
        ..Extractor::default()
    };
    extractor.rectangular_clips.push(true);
    extractor.opaque_groups.push(true);
    extractor.page.width = f64::from(width);
    extractor.page.height = f64::from(height);
    extractor
        .clips
        .push(Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
    interpret_page(page, &mut context, &mut extractor);
    // Form MCIDs have their own scope. The interpreter callback does not expose
    // that scope, so never guess a page structure association when Forms exist.
    let mut resources = Some(page.resources());
    let mut has_forms = false;
    while let Some(current) = resources {
        if current.x_objects.keys().any(|key| {
            current
                .x_objects
                .get::<Stream<'_>>(key.as_ref())
                .is_some_and(|s| {
                    s.dict()
                        .get::<Name<'_>>(b"Subtype")
                        .is_some_and(|n| n.as_ref() == b"Form")
                })
        }) {
            has_forms = true;
            break;
        }
        resources = current.parent();
    }
    if !has_forms {
        for glyph in &mut extractor.page.glyphs {
            if let Some(mcid) = glyph.mcid
                && let Some(Some(role)) = tags.get(&(index, mcid))
            {
                glyph.tag = Some(role.clone());
            }
        }
    }
    // URI rectangles use PDF coordinates; map with the exact page transform
    // used for extraction (including CropBox and page rotation).
    if let Some(annotations) = page.raw().get::<Array<'_>>(b"Annots") {
        for annotation in annotations.iter::<Dict<'_>>() {
            let Some(action) = annotation.get::<Dict<'_>>(b"A") else {
                continue;
            };
            if action
                .get::<Name<'_>>(b"S")
                .is_none_or(|n| n.as_ref() != b"URI")
            {
                continue;
            }
            let Some(uri) = action.get::<PdfString<'_>>(b"URI") else {
                continue;
            };
            let uri = String::from_utf8_lossy(uri.as_bytes()).into_owned();
            if !uri.starts_with("https://") && !uri.starts_with("http://") {
                continue;
            }
            let Some(rect) = annotation.get::<Array<'_>>(b"Rect") else {
                continue;
            };
            let values: Vec<f64> = rect
                .iter::<hayro::hayro_syntax::object::Number>()
                .map(|n| n.as_f64())
                .collect();
            if values.len() != 4 {
                continue;
            }
            let bounds = page
                .initial_transform(true)
                .to_kurbo()
                .transform_rect_bbox(Rect::new(values[0], values[1], values[2], values[3]));
            for glyph in &mut extractor.page.glyphs {
                if bounds.contains(glyph.bounds().center()) {
                    glyph.link = Some(uri.clone());
                }
            }
        }
    }
    extractor.page
}

fn array(rect: Rect) -> [f64; 4] {
    [rect.x0, rect.y0, rect.x1, rect.y1]
}

impl Device<'_> for Extractor {
    fn set_soft_mask(&mut self, mask: Option<SoftMask<'_>>) {
        self.soft_mask = mask.is_some();
    }
    fn set_blend_mode(&mut self, mode: BlendMode) {
        self.normal_blend = mode == BlendMode::Normal;
    }
    fn push_transparency_group(
        &mut self,
        opacity: f32,
        mask: Option<SoftMask<'_>>,
        mode: BlendMode,
    ) {
        self.opaque_groups.push(
            self.opaque_groups.last().copied().unwrap_or(true)
                && opacity == 1.0
                && mask.is_none()
                && mode == BlendMode::Normal
                && !self.soft_mask
                && self.normal_blend,
        );
    }
    fn pop_transparency_group(&mut self) {
        self.opaque_groups.pop();
    }
    fn push_clip_path(&mut self, clip: &ClipPath) {
        let bounds = clip.path.bounding_box();
        self.rectangular_clips.push(
            self.rectangular_clips.last().copied().unwrap_or(true) && rectangular_clip(&clip.path),
        );
        self.clips.push(
            self.clips
                .last()
                .copied()
                .map_or(bounds, |old| old.intersect(bounds)),
        );
    }
    fn pop_clip_path(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
            self.rectangular_clips.pop();
        }
    }
    fn begin_marked_content(&mut self, tag: &[u8], mcid: Option<i32>) {
        self.tags
            .push((String::from_utf8_lossy(tag).into_owned(), mcid));
    }
    fn end_marked_content(&mut self) {
        self.tags.pop();
    }

    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'_>,
        transform: Affine,
        glyph_transform: Affine,
        _: &Paint<'_>,
        mode: &GlyphDrawMode,
    ) {
        if matches!(mode, GlyphDrawMode::Invisible) {
            self.page.invisible += 1;
        }
        let unicode = glyph.as_unicode();
        let unmapped = unicode.is_none();
        if unmapped {
            self.page.unmapped += 1;
        }
        let text = match unicode {
            Some(BfString::Char(c)) => c.to_string(),
            Some(BfString::String(s)) => s,
            None => "\u{fffc}".into(),
        }
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
        if text.is_empty() {
            return;
        }
        let combined = transform * glyph_transform;
        let baseline = combined * Point::ORIGIN;
        let advance = combined * Point::new(glyph_advance(glyph), 0.0);
        let rect = fallback_glyph_rect(glyph, combined);
        if !rect_is_finite(rect)
            || self
                .clips
                .last()
                .is_some_and(|clip| !clip.contains(rect.center()))
        {
            return;
        }
        let matrix = combined.as_coeffs();
        let size = matrix[2].hypot(matrix[3]) * 1000.0;
        if !size.is_finite() || !(0.5..=500.0).contains(&size) {
            self.page.unmapped += 1;
            return;
        }
        let (bold, italic) = if let Glyph::Outline(g) = glyph {
            *self.fonts.entry(g.font_cache_key()).or_insert_with(|| {
                g.font_data().map_or((false, false), |data| {
                    (
                        data.weight.is_some_and(|w| w >= 600)
                            || data
                                .postscript_name
                                .as_deref()
                                .is_some_and(|n| n.to_ascii_lowercase().contains("bold")),
                        data.is_italic,
                    )
                })
            })
        } else {
            (false, false)
        };
        if self.page.glyphs.last().is_some_and(|old| {
            old.text == text
                && old
                    .rect
                    .iter()
                    .zip(array(rect))
                    .all(|(a, b)| (*a - b).abs() < 0.01)
        }) {
            return;
        }
        let tag = self.tags.iter().rev().find(|(t, _)| t != "Span");
        self.page.glyphs.push(NativeGlyph {
            text,
            rect: array(rect),
            baseline: [baseline.x, baseline.y],
            advance: [advance.x, advance.y],
            size,
            bold,
            italic,
            rotated: matrix[1].abs() > matrix[0].abs() * 0.15 || matrix[0] <= 0.0,
            tag: tag.map(|(t, _)| t.clone()),
            mcid: self.tags.iter().rev().find_map(|(_, id)| *id),
            link: None,
            index: self.page.glyphs.len(),
            unmapped,
        });
    }

    fn draw_image(&mut self, image: Image<'_, '_>, transform: Affine) {
        let bounds = transform.transform_rect_bbox(Rect::new(
            0.0,
            0.0,
            image.width() as f64,
            image.height() as f64,
        ));
        let clipped = self
            .clips
            .last()
            .map_or(bounds, |clip| bounds.intersect(*clip));
        if rect_is_finite(clipped) && clipped.width() > 2.0 && clipped.height() > 2.0 {
            if matches!(&image, Image::Raster(_)) {
                self.page.raster_decode_bytes = self.page.raster_decode_bytes.saturating_add(
                    (image.width() as usize)
                        .saturating_mul(image.height() as usize)
                        .saturating_mul(8),
                );
            }
            let matrix = transform.as_coeffs();
            let plain = self.rectangular_clips.last().copied().unwrap_or(false)
                && self.opaque_groups.last().copied().unwrap_or(false)
                && !self.soft_mask
                && self.normal_blend
                && matrix[0] > 0.0
                && matrix[3] > 0.0
                && matrix[1].abs() < 1e-8
                && matrix[2].abs() < 1e-8
                && array(bounds)
                    .iter()
                    .zip(array(clipped))
                    .all(|(a, b)| (*a - b).abs() < 1e-6);
            let encoded = match image {
                Image::Raster(raster) if plain => {
                    simple_image(raster.stream(), raster.width(), raster.height())
                }
                _ => None,
            };
            self.page.images.push(array(clipped));
            self.page.encoded_images.push(encoded);
        }
    }

    fn draw_path(&mut self, path: &BezPath, transform: Affine, _: &Paint<'_>, mode: &PathDrawMode) {
        let padding = match mode {
            PathDrawMode::Stroke(props) => {
                f64::from(props.line_width) * max_scale(transform) * 8.0 + 2.0
            }
            _ => 2.0,
        };
        self.page.image_obstacles.push(array(
            transform
                .transform_rect_bbox(path.bounding_box())
                .inflate(padding, padding),
        ));
        let mut current = None;
        let mut start = None;
        let mut curved = false;
        for element in path.elements() {
            match *element {
                kurbo::PathEl::MoveTo(point) => {
                    current = Some(transform * point);
                    start = current;
                }
                kurbo::PathEl::LineTo(point) => {
                    let next = transform * point;
                    if current.is_some_and(|old| {
                        (old.x - next.x).abs() > 1.0 && (old.y - next.y).abs() > 1.0
                    }) {
                        curved = true;
                    }
                    self.rule(current, next);
                    current = Some(next);
                }
                kurbo::PathEl::ClosePath => {
                    if let Some(first) = start {
                        self.rule(current, first);
                    }
                }
                _ => {
                    curved = true;
                }
            }
        }
        let bounds = transform.transform_rect_bbox(path.bounding_box());
        // Ignore page backgrounds and tiny ornament strokes. Curved/vector art
        // remains authoritative as an image if it cannot become semantic text.
        if curved
            && bounds.width() > 4.0
            && bounds.height() > 4.0
            && bounds.area() < self.page.width * self.page.height * 0.8
        {
            self.page.graphics.push(array(bounds));
        }
    }
}

impl Extractor {
    fn rule(&mut self, start: Option<Point>, end: Point) {
        let Some(start) = start else { return };
        if ((start.x - end.x).abs() < 0.5 && (start.y - end.y).abs() > 8.0)
            || ((start.y - end.y).abs() < 0.5 && (start.x - end.x).abs() > 8.0)
        {
            self.page.rules.push([
                start.x.min(end.x),
                start.y.min(end.y),
                start.x.max(end.x),
                start.y.max(end.y),
            ]);
        }
    }
}

// A bounding rectangle alone does not prove that a clip is rectangular.
fn rectangular_clip(path: &BezPath) -> bool {
    let elements = path.elements();
    let bounds = path.bounding_box();
    if elements.len() != 5 || !matches!(elements[4], kurbo::PathEl::ClosePath) {
        return false;
    }
    let mut points = Vec::new();
    for element in &elements[..4] {
        match element {
            kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) => points.push(*p),
            _ => return false,
        }
    }
    points
        .iter()
        .all(|p| (p.x == bounds.x0 || p.x == bounds.x1) && (p.y == bounds.y0 || p.y == bounds.y1))
        && (0..4).all(|i| {
            let a = points[i];
            let b = points[(i + 1) % 4];
            a != b && ((a.x == b.x) != (a.y == b.y))
        })
}

fn max_scale(transform: Affine) -> f64 {
    let m = transform.as_coeffs();
    m[0].hypot(m[1]).max(m[2].hypot(m[3]))
}

fn simple_image(stream: &Stream<'_>, width: u32, height: u32) -> Option<EncodedImage> {
    let dict = stream.dict();
    let id = dict.obj_id()?;
    // Isolated raster pixels still use the PDF decoder for their color space,
    // decode array and interpolation. Masks/compositing require page rendering.
    if id.obj_number <= 0
        || width == 0
        || height == 0
        || width.max(height) > 2048
        || [b"Mask".as_slice(), b"SMask", b"SMaskInData"]
            .iter()
            .any(|key| dict.get::<Object<'_>>(key).is_some())
    {
        return None;
    }
    Some(EncodedImage {
        object: [id.obj_number, id.gen_number],
        width,
        height,
    })
}
