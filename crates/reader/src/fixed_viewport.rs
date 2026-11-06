//! Fixed-page view geometry shared by hosts. Source coordinates never change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FixedPageTransform {
    pub scale: f32,
    pub offset: [f32; 2],
}
impl Default for FixedPageTransform {
    fn default() -> Self {
        Self {
            scale: 1.,
            offset: [0., 0.],
        }
    }
}
impl FixedPageTransform {
    pub fn to_view(self, p: [f32; 2]) -> [f32; 2] {
        [
            p[0] * self.scale + self.offset[0],
            p[1] * self.scale + self.offset[1],
        ]
    }
    pub fn to_document(self, p: [f32; 2]) -> [f32; 2] {
        [
            (p[0] - self.offset[0]) / self.scale,
            (p[1] - self.offset[1]) / self.scale,
        ]
    }
    pub fn zoom_at(&mut self, scale: f32, anchor: [f32; 2]) {
        if !scale.is_finite() || scale <= 0. || anchor.iter().any(|x| !x.is_finite()) {
            return;
        }
        let source = self.to_document(anchor);
        self.scale = scale.clamp(0.05, 32.);
        self.offset = [
            anchor[0] - source[0] * self.scale,
            anchor[1] - source[1] * self.scale,
        ];
    }
    pub fn constrain(&mut self, bounds: [f32; 4], viewport: [f32; 2]) {
        for axis in 0..2 {
            let start = bounds[axis] * self.scale;
            let size = bounds[axis + 2] * self.scale;
            self.offset[axis] = if size <= viewport[axis] {
                (viewport[axis] - size) * 0.5 - start
            } else {
                self.offset[axis].clamp(viewport[axis] - start - size, -start)
            };
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zoom_retains_pointer_anchor_and_source_coordinates() {
        let mut t = FixedPageTransform::default();
        let before = t.to_document([300., 240.]);
        t.zoom_at(3., [300., 240.]);
        assert_eq!(t.to_document([300., 240.]), before);
        assert_eq!(t.to_document(t.to_view([80., 70.])), [80., 70.]);
        t.zoom_at(f32::NAN, [0., 0.]);
        assert_eq!(t.scale, 3.);
    }
    #[test]
    fn panning_centers_small_pages_and_keeps_large_page_edges_visible() {
        let mut t = FixedPageTransform {
            scale: 2.,
            offset: [900., -900.],
        };
        t.constrain([100., 50., 300., 400.], [800., 600.]);
        assert_eq!(t.offset, [-100., -300.]);
    }
}
