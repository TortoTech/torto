//! Optional diagnostics for Rust allocations, separate from OS/driver memory.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Opt-in system allocator wrapper. Does not allocate while recording counters.
pub struct CountingAllocator;

fn added(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: every operation forwards the original pointer/layout to System.
// Counters use atomics, never allocate, and do not alter allocation behavior.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded GlobalAlloc caller contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            added(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded GlobalAlloc caller contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            added(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded GlobalAlloc caller contract.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded GlobalAlloc caller contract.
        let result = unsafe { System.realloc(ptr, layout, new_size) };
        if !result.is_null() {
            if new_size >= layout.size() {
                added(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        result
    }
}

/// Requested live and peak Rust heap bytes; excludes allocator overhead,
/// native library allocations, memory mappings and driver/GPU allocations.
pub fn rust_allocation_bytes() -> (usize, usize) {
    (LIVE.load(Ordering::Relaxed), PEAK.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_growth_shrink_and_zeroed_allocations() {
        let before = rust_allocation_bytes().0;
        let original = Layout::from_size_align(16, 8).unwrap();
        // SAFETY: layouts are valid; each allocation is checked, resized with
        // its current layout and finally deallocated with the matching layout.
        unsafe {
            let ptr = CountingAllocator.alloc_zeroed(original);
            assert!(!ptr.is_null());
            assert_eq!(rust_allocation_bytes().0, before + 16);
            assert!((0..16).all(|offset| *ptr.add(offset) == 0));
            let ptr = CountingAllocator.realloc(ptr, original, 32);
            assert!(!ptr.is_null());
            assert_eq!(rust_allocation_bytes().0, before + 32);
            let grown = Layout::from_size_align(32, 8).unwrap();
            let ptr = CountingAllocator.realloc(ptr, grown, 8);
            assert!(!ptr.is_null());
            assert_eq!(rust_allocation_bytes().0, before + 8);
            CountingAllocator.dealloc(ptr, Layout::from_size_align(8, 8).unwrap());
        }
        assert_eq!(rust_allocation_bytes().0, before);
    }
}
