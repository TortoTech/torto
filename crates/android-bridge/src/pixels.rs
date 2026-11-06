use crate::PageInfo;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicU64, Ordering},
};

pub(crate) fn registry() -> &'static Mutex<HashMap<u64, Weak<MobilePage>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, Weak<MobilePage>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}
#[derive(uniffi::Object)]
pub struct MobilePage {
    id: u64,
    width: u32,
    height: u32,
    pub(crate) pixels: Arc<Vec<u8>>,
    raster_id: u64,
    info: PageInfo,
}
impl MobilePage {
    pub(crate) fn new(width: u32, height: u32, pixels: Vec<u8>, info: PageInfo) -> Arc<Self> {
        Self::assemble(width, height, Arc::new(pixels), None, info)
    }
    fn assemble(
        width: u32,
        height: u32,
        pixels: Arc<Vec<u8>>,
        raster_id: Option<u64>,
        info: PageInfo,
    ) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let page = Arc::new(Self {
            id,
            width,
            height,
            pixels,
            raster_id: raster_id.unwrap_or(id),
            info,
        });
        registry()
            .lock()
            .expect("page registry")
            .insert(page.id, Arc::downgrade(&page));
        page
    }

    pub(crate) fn retained(raster: &Arc<Self>, info: PageInfo) -> Arc<Self> {
        Self::assemble(
            raster.width,
            raster.height,
            Arc::clone(&raster.pixels),
            Some(raster.raster_id),
            info,
        )
    }
}
#[uniffi::export]
impl MobilePage {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn raster_id(&self) -> u64 {
        self.raster_id
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn info(&self) -> PageInfo {
        self.info.clone()
    }
}
impl Drop for MobilePage {
    fn drop(&mut self) {
        if let Ok(mut registry) = registry().lock() {
            registry.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_frames_share_pixels_but_keep_independent_metadata_and_registry_lifetimes() {
        let raster = MobilePage::new(1, 1, vec![255; 4], PageInfo::default());
        let frame = MobilePage::retained(
            &raster,
            PageInfo {
                scroll_distance: 120.0,
                ..Default::default()
            },
        );
        let raster_id = raster.id();
        let frame_id = frame.id();
        assert_ne!(raster_id, frame_id);
        assert_eq!(raster.raster_id(), frame.raster_id());
        assert!(Arc::ptr_eq(&raster.pixels, &frame.pixels));
        assert_eq!(raster.info().scroll_distance, 0.0);
        assert_eq!(frame.info().scroll_distance, 120.0);
        drop(raster);
        assert!(!registry().lock().unwrap().contains_key(&raster_id));
        assert_eq!(frame.pixels.as_slice(), &[255; 4]);
        assert!(
            registry()
                .lock()
                .unwrap()
                .get(&frame_id)
                .unwrap()
                .upgrade()
                .is_some()
        );
        drop(frame);
        assert!(!registry().lock().unwrap().contains_key(&frame_id));
    }
}
