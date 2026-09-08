use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use rebook_reader::ReaderSpread;

use super::compositor::ReaderCompositor;
use super::scene::{SpreadSceneKey, StaticSpreadLayers};

pub const DEFAULT_SCENE_CACHE_CAPACITY: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceProfile {
    Low,
    Balanced,
    High,
}

impl ResourceProfile {
    pub const fn compositor_capacity(self) -> usize {
        match self {
            Self::Low => 4,
            Self::Balanced => 16,
            Self::High => 64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPressure {
    Moderate,
    Critical,
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheMetrics {
    pub hits: usize,
    pub misses: usize,
    pub builds: usize,
    pub evictions: usize,
}

pub struct SpreadSceneCache {
    capacity: usize,
    entries: HashMap<SpreadSceneKey, Arc<StaticSpreadLayers>>,
    lru: VecDeque<SpreadSceneKey>,
    pinned_keys: HashSet<SpreadSceneKey>,
    metrics: CacheMetrics,
}

impl SpreadSceneCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(2),
            entries: HashMap::new(),
            lru: VecDeque::new(),
            pinned_keys: HashSet::new(),
            metrics: CacheMetrics::default(),
        }
    }

    pub fn with_profile(profile: ResourceProfile) -> Self {
        Self::new(profile.compositor_capacity())
    }

    pub fn handle_memory_pressure(&mut self, pressure: MemoryPressure) {
        match pressure {
            MemoryPressure::Moderate => {
                // Drop unpinned entries beyond half of current capacity
                let target_capacity = (self.capacity / 2).max(2);
                while self.entries.len() > target_capacity {
                    let mut evicted = false;
                    for i in 0..self.lru.len() {
                        let candidate = &self.lru[i];
                        if !self.pinned_keys.contains(candidate) {
                            if let Some(key) = self.lru.remove(i) {
                                self.entries.remove(&key);
                                self.metrics.evictions += 1;
                                evicted = true;
                            }
                            break;
                        }
                    }
                    if !evicted {
                        break;
                    }
                }
            }
            MemoryPressure::Critical => {
                // Keep only pinned entries (e.g. current spread and prepared destination)
                let pinned = self.pinned_keys.clone();
                let keys_to_remove: Vec<SpreadSceneKey> = self
                    .entries
                    .keys()
                    .filter(|k| !pinned.contains(k))
                    .cloned()
                    .collect();
                for key in keys_to_remove {
                    self.entries.remove(&key);
                    self.metrics.evictions += 1;
                }
                self.lru.retain(|k| pinned.contains(k));
            }
        }
    }

    pub fn get_or_build(
        &mut self,
        key: &SpreadSceneKey,
        spread: &ReaderSpread,
    ) -> Arc<StaticSpreadLayers> {
        if let Some(existing) = self.entries.get(key) {
            self.metrics.hits += 1;
            let result = Arc::clone(existing);
            self.touch(key);
            return result;
        }

        self.metrics.misses += 1;
        self.metrics.builds += 1;

        let layers = Arc::new(ReaderCompositor::build_static_layers(spread, key.clone()));
        self.insert(key.clone(), Arc::clone(&layers));
        layers
    }

    pub fn pin(&mut self, key: SpreadSceneKey) {
        self.pinned_keys.insert(key);
    }

    pub fn unpin(&mut self, key: &SpreadSceneKey) {
        self.pinned_keys.remove(key);
    }

    pub fn clear_pins(&mut self) {
        self.pinned_keys.clear();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.pinned_keys.clear();
    }

    pub fn metrics(&self) -> &CacheMetrics {
        &self.metrics
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn touch(&mut self, key: &SpreadSceneKey) {
        if let Some(idx) = self.lru.iter().position(|k| k == key) {
            self.lru.remove(idx);
        }
        self.lru.push_back(key.clone());
    }

    fn insert(&mut self, key: SpreadSceneKey, layers: Arc<StaticSpreadLayers>) {
        self.touch(&key);
        self.entries.insert(key, layers);
        self.evict_if_needed();
    }

    fn evict_if_needed(&mut self) {
        while self.entries.len() > self.capacity {
            let mut evicted = false;
            for i in 0..self.lru.len() {
                let candidate = &self.lru[i];
                if !self.pinned_keys.contains(candidate) {
                    if let Some(key) = self.lru.remove(i) {
                        self.entries.remove(&key);
                        self.metrics.evictions += 1;
                        evicted = true;
                    }
                    break;
                }
            }
            if !evicted {
                // All entries are pinned; cannot evict further
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::scene::PageSceneKey;
    use rebook_layout::{LayoutViewport, PageLayout};
    use rebook_publication::Rgba;
    use rebook_reader::ReaderPosition;
    use rebook_renderer::DisplayListCompiler;

    fn make_test_spread() -> ReaderSpread {
        let compiler = DisplayListCompiler;
        let page_layout = PageLayout {
            viewport: LayoutViewport {
                width: 800,
                height: 1000,
            },
            background: Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
            leading_gap: 0.0,
            items: Vec::new(),
        };
        let page_display_list = Arc::new(compiler.compile(&page_layout));
        ReaderSpread {
            primary: page_display_list,
            secondary: None,
            primary_offset_x: 0.0,
            secondary_offset_x: 0.0,
        }
    }

    fn make_key(page_index: usize, generation: u64) -> SpreadSceneKey {
        SpreadSceneKey {
            primary: PageSceneKey {
                position: ReaderPosition {
                    section_index: 0,
                    segment_index: 0,
                    page_index,
                },
                layout_generation: generation,
            },
            secondary: None,
            width: 800,
            height: 1000,
        }
    }

    #[test]
    fn cache_records_hits_misses_and_evictions() {
        let mut cache = SpreadSceneCache::new(2);
        let spread = make_test_spread();

        let k1 = make_key(0, 0);
        let k2 = make_key(1, 0);
        let k3 = make_key(2, 0);

        let _ = cache.get_or_build(&k1, &spread);
        assert_eq!(cache.metrics().misses, 1);
        assert_eq!(cache.metrics().hits, 0);
        assert_eq!(cache.metrics().builds, 1);

        let _ = cache.get_or_build(&k1, &spread);
        assert_eq!(cache.metrics().hits, 1);
        assert_eq!(cache.metrics().builds, 1);

        let _ = cache.get_or_build(&k2, &spread);
        assert_eq!(cache.len(), 2);

        let _ = cache.get_or_build(&k3, &spread);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.metrics().evictions, 1);

        cache.pin(k2.clone());
        let _ = cache.get_or_build(&k1, &spread);
        assert!(cache.entries.contains_key(&k2));
    }

    #[test]
    fn memory_pressure_evicts_unpinned_entries() {
        let mut cache = SpreadSceneCache::new(10);
        let spread = make_test_spread();

        let k1 = make_key(0, 0);
        let k2 = make_key(1, 0);
        let k3 = make_key(2, 0);

        let _ = cache.get_or_build(&k1, &spread);
        let _ = cache.get_or_build(&k2, &spread);
        let _ = cache.get_or_build(&k3, &spread);
        assert_eq!(cache.len(), 3);

        cache.pin(k2.clone());
        cache.handle_memory_pressure(MemoryPressure::Critical);

        // Only pinned k2 should remain
        assert_eq!(cache.len(), 1);
        assert!(cache.entries.contains_key(&k2));
    }
}
