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
    pub(crate) pixels: Vec<u8>,
    info: PageInfo,
}
impl MobilePage {
    pub(crate) fn new(width: u32, height: u32, pixels: Vec<u8>, info: PageInfo) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let page = Arc::new(Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            pixels,
            info,
        });
        registry()
            .lock()
            .expect("page registry")
            .insert(page.id, Arc::downgrade(&page));
        page
    }
}
#[uniffi::export]
impl MobilePage {
    pub fn id(&self) -> u64 {
        self.id
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
